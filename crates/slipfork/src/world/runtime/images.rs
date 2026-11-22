//! Image collaborator: `from` pulls and `Dockerfile` builds into sealed, read-only rootfs clones.
//!
//! Independent images pull and build concurrently with no global lock; per-key singleflight slots
//! deduplicate concurrent requests for the same image id or OCI reference.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout,
};
use slipfork_world::pod_log;
use slipfork_world::{BootState, ImageNode, ImageStamp, StoreLayout, WorldEngine, WorldName};

use crate::world::supervise::{BootSink, strip_ansi};

/// One sealed image plus the OCI metadata every pod derived from it inherits.
#[derive(Clone)]
pub(super) struct BuiltImage {
    /// Read-only rootfs subvolume drawn from the cache.
    pub(super) rootfs: PathBuf,
    /// Image `config.Env` entries, inherited verbatim.
    pub(super) inherited_env: Vec<String>,
    /// `ENTRYPOINT` followed by `CMD`; empty when the image defines neither.
    pub(super) argv: Vec<String>,
    /// Image `config.WorkingDir`.
    pub(super) workdir: String,
}

enum FlightState<T> {
    Running,
    Done(Result<T, String>),
}

type Flight<T> = Arc<(Mutex<FlightState<T>>, Condvar)>;
type FlightTable<T> = Arc<Mutex<BTreeMap<String, Flight<T>>>>;

struct FlightGuard<T: Clone> {
    key: String,
    table: FlightTable<T>,
    flight: Flight<T>,
    completed: bool,
}

impl<T: Clone> FlightGuard<T> {
    fn finish(mut self, outcome: Result<T, String>) {
        self.completed = true;
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.key);
        }
        let (lock, cvar) = &*self.flight;
        if let Ok(mut slot) = lock.lock() {
            *slot = FlightState::Done(outcome);
            cvar.notify_all();
        }
    }
}

impl<T: Clone> Drop for FlightGuard<T> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.key);
        }
        let (lock, cvar) = &*self.flight;
        if let Ok(mut slot) = lock.lock() {
            *slot = FlightState::Done(Err("an image worker panicked".to_owned()));
            cvar.notify_all();
        }
    }
}

/// The sealed-image cache plus its puller and native builder.
pub(super) struct ImageCache {
    world: WorldName,
    project: PathBuf,
    doc: Forkfile,
    puller: OciPuller,
    builder: NativeImageBuilder,
    images: Vec<ImageNode>,
    logs_root: PathBuf,
    sealed: Mutex<BTreeMap<String, BuiltImage>>,
    pulled: Mutex<BTreeMap<String, PulledImage>>,
    inflight_builds: FlightTable<(BuiltImage, BootState)>,
    inflight_pulls: FlightTable<PulledImage>,
}

impl ImageCache {
    /// Builds the cache; `adopted` carries a parent fork's already-sealed image subvolumes and
    /// runtime facts, so a matching `world_epoch` resolves every image in `~1ms` as `cached`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        engine: Arc<dyn WorldEngine>,
        layout: &StoreLayout,
        world: WorldName,
        project: &Path,
        doc: &Forkfile,
        images: Vec<ImageNode>,
        adopted: BTreeMap<String, ImageStamp>,
        architecture: Architecture,
        agent: &str,
        host: StepHost,
    ) -> Self {
        let credentials = Credentials::Anonymous;
        let images_root = layout.images_root();
        let mut sealed = BTreeMap::new();
        for (id, stamp) in adopted {
            if !images.iter().any(|node| node.id == id) {
                continue;
            }
            if !engine.is_sealed(Path::new(&stamp.rootfs)).unwrap_or(false) {
                // A stamp whose subvolume disappeared falls back to the normal build path.
                continue;
            }
            sealed.insert(
                id,
                BuiltImage {
                    rootfs: PathBuf::from(stamp.rootfs),
                    inherited_env: stamp.env,
                    argv: stamp.argv,
                    workdir: stamp.workdir,
                },
            );
        }
        let puller = OciPuller::new(
            Arc::clone(&engine),
            ImageLayout::new(images_root.clone()),
            credentials.clone(),
            architecture,
            agent.to_owned(),
        );
        let build_puller = OciPuller::new(
            Arc::clone(&engine),
            ImageLayout::new(images_root.clone()),
            credentials,
            architecture,
            agent.to_owned(),
        )
        .sharing_flights(&puller);
        let builder = NativeImageBuilder::new(
            engine,
            build_puller,
            StepLayout::new(images_root),
            architecture,
            host,
        );
        let logs_root = layout.world_logs(&world);
        Self {
            world,
            project: project.to_path_buf(),
            doc: doc.clone(),
            puller,
            builder,
            images,
            logs_root,
            sealed: Mutex::new(sealed),
            pulled: Mutex::new(BTreeMap::new()),
            inflight_builds: Arc::new(Mutex::new(BTreeMap::new())),
            inflight_pulls: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Seals one image and reports whether it was cached, pulled, or built.
    pub(super) fn build(
        &self,
        image: &str,
        log: &mut BootSink,
    ) -> Result<(BuiltImage, BootState), String> {
        if let Some(built) = self
            .sealed
            .lock()
            .map_err(|_| "image cache lock poisoned".to_owned())?
            .get(image)
            .cloned()
        {
            return Ok((built, BootState::Cached));
        }
        let guard = match acquire_flight(&self.inflight_builds, image.to_owned())? {
            FlightRole::Wait(flight) => {
                // A waiter inherits the leader's real state: a singleflight hit is not a build.
                return wait_flight(&flight);
            }
            FlightRole::Lead(guard) => guard,
        };
        if let Some(built) = self
            .sealed
            .lock()
            .map_err(|_| "image cache lock poisoned".to_owned())?
            .get(image)
            .cloned()
        {
            let outcome = Ok((built, BootState::Cached));
            guard.finish(outcome.clone());
            return outcome;
        }
        let outcome = self.build_uncached(image, log);
        if let Ok((built, _)) = &outcome
            && let Ok(mut sealed) = self.sealed.lock()
        {
            sealed.insert(image.to_owned(), built.clone());
        }
        guard.finish(outcome.clone());
        outcome
    }

    fn build_uncached(
        &self,
        image: &str,
        log: &mut BootSink,
    ) -> Result<(BuiltImage, BootState), String> {
        let node = self
            .images
            .iter()
            .find(|candidate| candidate.id == image)
            .cloned()
            .ok_or_else(|| format!("unknown image `{image}`"))?;
        if let Some(reference) = &node.from {
            let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
            let mut tee = BuildLog::new(log, &self.logs_root, image);
            let pulled = self.puller.pull_into(&parsed, &mut tee).map_err(|error| {
                format!("pull `{reference}` failed: {error}\nnext: run `slipfork logs {image}`")
            })?;
            tee.finish();
            let state = if pulled.cached {
                BootState::Cached
            } else {
                BootState::Pulled
            };
            let mut argv = pulled.facts.entrypoint;
            argv.extend(pulled.facts.cmd);
            Ok((
                BuiltImage {
                    rootfs: pulled.rootfs,
                    inherited_env: pulled.facts.env,
                    argv,
                    workdir: "/".to_owned(),
                },
                state,
            ))
        } else {
            let context = self.project.join(&node.directory);
            let dockerfile = context.join(
                node.dockerfile
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("Dockerfile")),
            );
            let mut args = BuildArgs::new();
            for (key, value) in self.doc.eval_image_args(self.world.as_str(), image) {
                args.declare(key, value);
            }
            let request = BuildRequest {
                context,
                dockerfile,
                args,
                target: self.doc.eval_image_stage(image),
            };
            let mut tee = BuildLog::new(log, &self.logs_root, image);
            let report = self.builder.build(&request, &mut tee).map_err(|error| {
                format!("build `{image}` failed: {error}\nnext: run `slipfork logs {image}`")
            })?;
            tee.finish();
            let _ = writeln!(
                log,
                "dockerfile {image}: {} cached, {} built",
                report.cache_hits, report.cache_misses
            );
            let _ = log.flush();
            // A sealed step cache hit is `cached`; only an actually applied step is `built`. A
            // metadata-only `FROM`+`ENV` Dockerfile has zero hits, so it is still a real build.
            let state = if report.cache_hits > 0 && report.cache_misses == 0 {
                BootState::Cached
            } else {
                BootState::Built
            };
            Ok((
                BuiltImage {
                    rootfs: report.rootfs,
                    inherited_env: report
                        .config
                        .env
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect(),
                    argv: report.config.exec_argv(),
                    workdir: report.config.workdir,
                },
                state,
            ))
        }
    }

    /// Pulls one resource image (`postgres:18`, `redis:7`) once and reuses it afterwards.
    pub(super) fn pull(&self, reference: &str, log: &mut BootSink) -> Result<PulledImage, String> {
        if let Some(image) = self
            .pulled
            .lock()
            .map_err(|_| "pulled image lock poisoned".to_owned())?
            .get(reference)
            .cloned()
        {
            return Ok(image);
        }
        let guard = match acquire_flight(&self.inflight_pulls, reference.to_owned())? {
            FlightRole::Wait(flight) => return wait_flight(&flight),
            FlightRole::Lead(guard) => guard,
        };
        if let Some(image) = self
            .pulled
            .lock()
            .map_err(|_| "pulled image lock poisoned".to_owned())?
            .get(reference)
            .cloned()
        {
            let outcome = Ok(image);
            guard.finish(outcome.clone());
            return outcome;
        }
        let outcome = ImageReference::parse(reference)
            .map_err(|error| error.to_string())
            .and_then(|parsed| {
                self.puller
                    .pull_into(&parsed, log)
                    .map_err(|error| error.to_string())
            });
        if let Ok(image) = &outcome
            && let Ok(mut pulled) = self.pulled.lock()
        {
            pulled.insert(reference.to_owned(), image.clone());
        }
        guard.finish(outcome.clone());
        outcome
    }

    /// Resolved rootfs + runtime facts per image id, recorded in the world stamp.
    pub(super) fn stamps(&self) -> BTreeMap<String, ImageStamp> {
        self.sealed
            .lock()
            .map(|sealed| {
                sealed
                    .iter()
                    .map(|(image, built)| {
                        (
                            image.clone(),
                            ImageStamp {
                                rootfs: built.rootfs.display().to_string(),
                                env: built.inherited_env.clone(),
                                argv: built.argv.clone(),
                                workdir: built.workdir.clone(),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A `Write` tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
struct BuildLog<'a> {
    sink: &'a mut BootSink,
    logs_root: PathBuf,
    id: String,
    pending: Vec<u8>,
}

impl<'a> BuildLog<'a> {
    fn new(sink: &'a mut BootSink, logs_root: &Path, id: &str) -> Self {
        Self {
            sink,
            logs_root: logs_root.to_path_buf(),
            id: id.to_owned(),
            pending: Vec::new(),
        }
    }

    /// Writes one cleaned line to the live sink and the `slipfork logs` capture.
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        let Some(text) = clean_line(line) else {
            return Ok(());
        };
        self.sink.write_all(text.as_bytes())?;
        let _ = pod_log::append(&self.logs_root, &self.id, text.as_bytes());
        Ok(())
    }

    /// Flushes a trailing unterminated line into the capture file.
    fn finish(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let _ = self.emit(&pending);
    }
}

impl Write for BuildLog<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buffer);
        let mut start = 0;
        while let Some(offset) = self.pending[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            let line = self.pending[start..end].to_vec();
            self.emit(&line)?;
            start = end + 1;
        }
        if start > 0 {
            self.pending.drain(..start);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

/// Strips CSI escapes and bare `\r` progress from one raw child line, so neither the `--json`
/// stream nor `<image>.log` ever carries terminal control bytes.
fn clean_line(bytes: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    let text = strip_ansi(&raw).replace('\r', "");
    (!text.trim().is_empty()).then_some(text)
}

enum FlightRole<T: Clone> {
    Lead(FlightGuard<T>),
    Wait(Flight<T>),
}

fn acquire_flight<T: Clone>(table: &FlightTable<T>, key: String) -> Result<FlightRole<T>, String> {
    let mut guard = table
        .lock()
        .map_err(|_| "singleflight lock poisoned".to_owned())?;
    if let Some(existing) = guard.get(&key) {
        return Ok(FlightRole::Wait(Arc::clone(existing)));
    }
    let flight = Arc::new((Mutex::new(FlightState::Running), Condvar::new()));
    guard.insert(key.clone(), Arc::clone(&flight));
    Ok(FlightRole::Lead(FlightGuard {
        key,
        table: Arc::clone(table),
        flight,
        completed: false,
    }))
}

fn wait_flight<T: Clone>(flight: &Flight<T>) -> Result<T, String> {
    let (lock, cvar) = &**flight;
    let mut guard = lock
        .lock()
        .map_err(|_| "singleflight state poisoned".to_owned())?;
    loop {
        match &*guard {
            FlightState::Running => {
                guard = cvar
                    .wait(guard)
                    .map_err(|_| "singleflight wait poisoned".to_owned())?;
            }
            FlightState::Done(outcome) => return outcome.clone(),
        }
    }
}
