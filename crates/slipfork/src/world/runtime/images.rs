//! Image collaborator: `from` pulls and `Dockerfile` builds into sealed, read-only rootfs clones.
//!
//! Independent images pull and build concurrently with no global lock; per-key singleflight slots
//! deduplicate concurrent requests for the same image id or OCI reference.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout,
};
use slipfork_world::{BootState, ImageNode, StoreLayout, WorldEngine, WorldName};

use crate::world::supervise::BootSink;

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
    sealed: Mutex<BTreeMap<String, BuiltImage>>,
    pulled: Mutex<BTreeMap<String, PulledImage>>,
    inflight_builds: FlightTable<(BuiltImage, BootState)>,
    inflight_pulls: FlightTable<PulledImage>,
}

impl ImageCache {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        engine: Arc<dyn WorldEngine>,
        layout: &StoreLayout,
        world: WorldName,
        project: &Path,
        doc: &Forkfile,
        images: Vec<ImageNode>,
        architecture: Architecture,
        agent: &str,
        host: StepHost,
    ) -> Self {
        let credentials = Credentials::Anonymous;
        let images_root = layout.images_root();
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
        );
        let builder = NativeImageBuilder::new(
            engine,
            build_puller,
            StepLayout::new(images_root),
            architecture,
            host,
        );
        Self {
            world,
            project: project.to_path_buf(),
            doc: doc.clone(),
            puller,
            builder,
            images,
            sealed: Mutex::new(BTreeMap::new()),
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
                return wait_flight(&flight).map(|(built, _)| (built, BootState::Cached));
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
            let pulled = self
                .puller
                .pull_into(&parsed, log)
                .map_err(|error| error.to_string())?;
            let mut argv = pulled.facts.entrypoint;
            argv.extend(pulled.facts.cmd);
            Ok((
                BuiltImage {
                    rootfs: pulled.rootfs,
                    inherited_env: pulled.facts.env,
                    argv,
                    workdir: "/".to_owned(),
                },
                BootState::Pulled,
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
            let report = self
                .builder
                .build(&request, log)
                .map_err(|error| error.to_string())?;
            let _ = writeln!(
                log,
                "dockerfile {image}: {} cached, {} built",
                report.cache_hits, report.cache_misses
            );
            let _ = log.flush();
            let state = if report.cache_misses == 0 && report.cache_hits > 0 {
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

    /// Resolved rootfs subvolume per image id, recorded in the world stamp.
    pub(super) fn paths(&self) -> BTreeMap<String, String> {
        self.sealed
            .lock()
            .map(|sealed| {
                sealed
                    .iter()
                    .map(|(image, built)| (image.clone(), built.rootfs.display().to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
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
