//! Image collaborator: `from` pulls and `Dockerfile` builds into sealed, read-only rootfs clones.
//!
//! Independent images pull and build concurrently with no global lock; per-key singleflight slots
//! ([`flight`]) deduplicate concurrent requests for the same image id or OCI reference.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout, StepLog, StepTrace,
};
use slipfork_world::run_log::RunNote;
use slipfork_world::{
    BootState, ImageNode, ImageStamp, LogStore, StoreLayout, WorldEngine, WorldName,
};

use crate::world::supervise::BootSink;

mod flight;
mod log;
pub(super) mod record;

use flight::{FlightRole, FlightTable, acquire_flight, wait_flight};
use log::{BuildLog, failed_note, pull_note, success_note};

pub(crate) use record::BuildFailure;

/// One sealed image plus the OCI metadata every pod derived from it inherits.
#[derive(Debug, Clone)]
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

/// The sealed-image cache plus its puller and native builder.
pub(super) struct ImageCache {
    world: WorldName,
    project: PathBuf,
    doc: Forkfile,
    scheme: String,
    features: FeatureSet,
    puller: OciPuller,
    builder: NativeImageBuilder,
    images: Vec<ImageNode>,
    store: LogStore,
    sealed: Mutex<BTreeMap<String, BuiltImage>>,
    pulled: Mutex<BTreeMap<String, PulledImage>>,
    /// The last instruction each image build applied, for the run record it publishes.
    traces: Mutex<BTreeMap<String, StepTrace>>,
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
        scheme: &str,
        features: FeatureSet,
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
        let store = LogStore::at(layout.store_root(), world.clone());
        Self {
            world,
            scheme: scheme.to_owned(),
            features,
            project: project.to_path_buf(),
            doc: doc.clone(),
            puller,
            builder,
            images,
            store,
            sealed: Mutex::new(sealed),
            pulled: Mutex::new(BTreeMap::new()),
            traces: Mutex::new(BTreeMap::new()),
            inflight_builds: Arc::new(Mutex::new(BTreeMap::new())),
            inflight_pulls: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Seals one image and reports whether it was cached, pulled, or built.
    pub(super) fn build(
        &self,
        image: &str,
        log: &mut BootSink,
    ) -> Result<(BuiltImage, BootState), BuildFailure> {
        if let Some(built) = self
            .sealed
            .lock()
            .map_err(|_| plain_failure("image cache lock poisoned".to_owned()))?
            .get(image)
            .cloned()
        {
            return Ok((built, BootState::Cached));
        }
        let guard =
            match acquire_flight(&self.inflight_builds, image.to_owned()).map_err(plain_failure)? {
                FlightRole::Wait(flight) => {
                    // A waiter inherits the leader's real state: a singleflight hit is not a build.
                    return wait_flight(&flight).map_err(plain_failure);
                }
                FlightRole::Lead(guard) => guard,
            };
        if let Some(built) = self
            .sealed
            .lock()
            .map_err(|_| plain_failure("image cache lock poisoned".to_owned()))?
            .get(image)
            .cloned()
        {
            let outcome: Result<_, BuildFailure> = Ok((built, BootState::Cached));
            guard.finish(outcome.clone().map_err(|failure| failure.message));
            return outcome;
        }
        let outcome = self.build_uncached(image, log);
        if let Ok((built, _)) = &outcome
            && let Ok(mut sealed) = self.sealed.lock()
        {
            sealed.insert(image.to_owned(), built.clone());
        }
        guard.finish(outcome.clone().map_err(|failure| failure.message));
        outcome
    }

    fn build_uncached(
        &self,
        image: &str,
        log: &mut BootSink,
    ) -> Result<(BuiltImage, BootState), BuildFailure> {
        let node = self
            .images
            .iter()
            .find(|candidate| candidate.id == image)
            .cloned()
            .ok_or_else(|| BuildFailure {
                message: format!("unknown image `{image}`"),
                note: RunNote::default(),
            })?;
        if let Some(reference) = &node.from {
            let parsed = ImageReference::parse(reference).map_err(|error| BuildFailure {
                message: error.to_string(),
                note: pull_note(reference),
            })?;
            let mut tee = BuildLog::new(log, &self.store, image);
            let pulled = match self.puller.pull_into(&parsed, &mut tee) {
                Ok(pulled) => {
                    tee.finish();
                    pulled
                }
                Err(error) => {
                    let message = format!("pull `{image}` failed: {error}");
                    tee.finish();
                    return Err(BuildFailure {
                        message,
                        note: pull_note(reference),
                    });
                }
            };
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
            for (key, value) in self.doc.eval_image_args_with(
                self.world.as_str(),
                &self.scheme,
                image,
                &self.features,
            ) {
                args.declare(key, value);
            }
            let request = BuildRequest {
                context,
                dockerfile,
                args,
                target: self.doc.eval_image_stage_with(image, &self.features),
            };
            let mut tee = BuildLog::new(log, &self.store, image);
            let built = {
                let mut sink = |stream: slipfork_image::Stream, line: &str| {
                    tee.append_stream(stream, line);
                };
                let mut step_log = StepLog::new(&mut sink);
                self.builder.build(&request, &mut step_log)
            };
            let report = match built {
                Ok(report) => {
                    tee.finish();
                    report
                }
                Err(error) => {
                    // Keep only real child output: flush a trailing unterminated tail, then record
                    // the failing instruction on the run record. The summary line stays in the
                    // `up` failure, never in the capture.
                    let message = format!("build `{image}` failed: {error}");
                    let note = failed_note(&request.dockerfile, &error);
                    tee.finish();
                    return Err(BuildFailure { message, note });
                }
            };
            if let Some(last) = &report.last
                && let Ok(mut traces) = self.traces.lock()
            {
                traces.insert(image.to_owned(), last.clone());
            }
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

    /// The instruction a successful build leaves on its run record.
    pub(super) fn success_note(&self, image: &str) -> RunNote {
        let last = self
            .traces
            .lock()
            .ok()
            .and_then(|traces| traces.get(image).cloned());
        success_note(&self.images, image, last.as_ref())
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

/// A failure with no executed instruction to name.
fn plain_failure(message: String) -> BuildFailure {
    BuildFailure {
        message,
        note: RunNote::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use slipfork_forkfile::parse;
    use slipfork_image::{Architecture, StepHost};
    use slipfork_world::{BtrfsEngine, ImageNode, StoreLayout, WorldEngine, WorldName};

    use super::ImageCache;
    use crate::world::supervise::{BootSink, Emitter};

    /// A build that fails before printing anything never fabricates a capture: the `up` failure
    /// carries the summary, while `logs <image>` has only real child output.
    #[test]
    fn a_failed_build_writes_no_synthetic_capture() {
        let root = tempfile::Builder::new()
            .prefix("image-cache-log")
            .tempdir_in("/var/tmp")
            .expect("disk-backed scratch root");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).expect("create the build context");
        std::fs::write(project.join("Dockerfile"), "FROM scratch\nRUN false\n")
            .expect("write the Dockerfile");
        let store = StoreLayout::new(root.path().join("store"));
        let engine = Arc::new(BtrfsEngine::new(store.clone()));
        let world = WorldName::parse("demo").expect("valid world name");
        let doc = parse("# forkfile: 0.1\n\nimage web . {\n  dockerfile Dockerfile\n}\n")
            .expect("the Forkfile parses");
        let cache = ImageCache::new(
            Arc::clone(&engine) as Arc<dyn WorldEngine>,
            &store,
            world.clone(),
            &project,
            &doc,
            vec![ImageNode {
                id: "web".to_owned(),
                directory: PathBuf::from("."),
                dockerfile: None,
                from: None,
                line: 3,
            }],
            BTreeMap::new(),
            Architecture::X86_64,
            "slipfork-tests/0.1",
            StepHost::new(
                "/nonexistent/crun",
                "/etc/resolv.conf",
                root.path().join("bundles"),
                1000,
                1000,
            ),
            "http",
            doc.default_features(),
        );
        let mut sink = BootSink::for_step(
            Arc::new(Mutex::new(Emitter::closed())),
            1,
            1,
            "image",
            "web",
        );
        let failure = cache
            .build("web", &mut sink)
            .expect_err("a missing crun must fail the image build");
        assert!(
            failure.message.contains("build `web` failed"),
            "{failure:?}"
        );
        assert!(
            !store.pod_log(&world, "web").exists(),
            "a run with no child output leaves no synthetic capture"
        );
    }
}
