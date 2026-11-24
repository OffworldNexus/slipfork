//! Image collaborator: `from` pulls and `Dockerfile` builds into sealed, read-only rootfs clones.
//!
//! Independent images pull and build concurrently with no global lock; per-key singleflight slots
//! ([`flight`]) deduplicate concurrent requests for the same image id or OCI reference.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, CacheMode, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, RegistryCredentials, StepHost, StepLayout,
    StepTrace,
};
use slipfork_world::run_log::RunNote;
use slipfork_world::{
    BootState, ImageNode, ImageStamp, LogStore, StoreLayout, WorldEngine, WorldName,
};

use crate::world::supervise::BootSink;

mod flight;
mod log;
mod pull;
pub(super) mod record;

pub(crate) use pull::pull_externals;

use flight::{FlightRole, FlightTable, acquire_flight, wait_flight};
use log::{BuildLog, failed_note, from_note, success_note};

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
    /// Container plumbing of the last failed build per image id, for the `-v`-only frame field.
    plumbing: Mutex<BTreeMap<String, String>>,
    inflight_builds: FlightTable<(BuiltImage, BootState)>,
    inflight_pulls: FlightTable<PulledImage>,
}

impl ImageCache {
    /// Builds the cache; `adopted` carries a parent fork's already-sealed image subvolumes and
    /// runtime facts, so a matching `world_epoch` resolves every image in `~1ms` as `cached`.
    /// `no_cache` ignores every adopted stamp and rebuilds in place: the cold step policy skips
    /// every sealed lookup and replaces each same-key subvolume instead.
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
        credentials: RegistryCredentials,
        agent: &str,
        host: StepHost,
        scheme: &str,
        features: FeatureSet,
        no_cache: bool,
    ) -> Self {
        let images_root = layout.images_root();
        let mut sealed = BTreeMap::new();
        // `--no-cache` ignores every adopted stamp: each image rebuilds from its `FROM` down.
        for (id, stamp) in adopted.into_iter().filter(|_| !no_cache) {
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
        )
        .with_cache_mode(if no_cache {
            CacheMode::Cold
        } else {
            CacheMode::Warm
        });
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
            plumbing: Mutex::new(BTreeMap::new()),
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
        if let Err(failure) = &outcome
            && let Some(plumbing) = &failure.runtime
            && let Ok(mut ledger) = self.plumbing.lock()
        {
            ledger.insert(image.to_owned(), plumbing.to_string());
        }
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
            .ok_or_else(|| plain_failure(format!("unknown image `{image}`")))?;
        if let Some(reference) = &node.from {
            let run_id = log.run_id();
            let mut tee = BuildLog::new(log, &self.store, run_id.as_deref(), image);
            let progress = tee.run(None, |step_log| {
                // The `FROM` row exists before the reference is even parsed, so an invalid
                // reference is a failure OF this step, not a step-less build failure.
                step_log.announce("FROM", reference);
                let started = std::time::Instant::now();
                let outcome = match ImageReference::parse(reference) {
                    Ok(parsed) => self.puller.pull_into(&parsed, step_log),
                    Err(error) => Err(error),
                };
                match &outcome {
                    Ok(_) => step_log.done(started.elapsed()),
                    Err(error) => step_log.failed(started.elapsed(), error.exit_code()),
                }
                outcome
            });
            if let Err(error) = &progress.outcome {
                // A pull the engine executes has no child process, so its raw one-line cause is
                // that step's stderr output rather than the wrapped summary `up` prints.
                let reason = tee
                    .explain(error, &progress)
                    .unwrap_or_else(|| error.cause());
                let message = format!("pull `{image}` failed: {reason}");
                let mut note = progress
                    .started
                    .clone()
                    .unwrap_or_else(|| from_note(reference));
                note.reason = Some(reason);
                return Err(BuildFailure {
                    message,
                    note: Box::new(note),
                    runtime: error.runtime().map(Into::into),
                });
            }
            let Ok(pulled) = progress.outcome else {
                unreachable!("the pull failure returned above")
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
            let paths = node.paths(&self.project);
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
                context: paths.context,
                dockerfile: paths.dockerfile,
                args,
                target: self.doc.eval_image_stage_with(image, &self.features),
                externals: BTreeMap::new(),
            };
            let run_id = log.run_id();
            let mut tee = BuildLog::new(log, &self.store, run_id.as_deref(), image);
            let progress = tee.run(Some(&request.dockerfile), |step_log| {
                // The builder never touches the network: every `COPY --from=<image>` rootfs is
                // pulled here first, then handed in keyed by the reference as the author wrote it.
                let externals = self.pull_externals(&request, step_log)?;
                let request = BuildRequest {
                    externals,
                    ..request.clone()
                };
                self.builder.build(&request, step_log)
            });
            if let Err(error) = &progress.outcome {
                // The engine writes its own one-line cause only when a step started and captured no
                // child output; a `RUN` keeps its child lines and `(exit N)`, and a failure before
                // step 1 keeps the reason for the verdict.
                let reason = tee.explain(error, &progress);
                let message = format!("build `{image}` failed: {}", error.cause());
                let mut note = progress
                    .started
                    .clone()
                    .unwrap_or_else(|| failed_note(&request.dockerfile));
                note.exit_code = note.exit_code.or(error.exit_code());
                // With no step row there is nowhere for a gutter line, so the verdict carries it.
                note.reason = reason.or_else(|| progress.started.is_none().then(|| error.cause()));
                return Err(BuildFailure {
                    message,
                    note: Box::new(note),
                    runtime: error.runtime().map(Into::into),
                });
            }
            let Ok(report) = progress.outcome else {
                unreachable!("the build failure returned above")
            };
            if let Some(last) = &report.last
                && let Ok(mut traces) = self.traces.lock()
            {
                traces.insert(image.to_owned(), last.clone());
            }
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

    /// The `-v`-only container plumbing a failed image build reported.
    pub(super) fn plumbing(&self, image: &str) -> Option<String> {
        self.plumbing.lock().ok()?.get(image).cloned()
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
        note: Box::new(RunNote::default()),
        runtime: None,
    }
}

#[cfg(test)]
mod tests;
