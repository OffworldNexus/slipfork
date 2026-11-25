//! Daemon-side image pre-plan: seal every world image into `WorldHeads` before a supervisor runs.
//!
//! The daemon owns every pull and Dockerfile build. `__supervise` only reads the recorded heads,
//! so a failed build never detaches or stops the serving world, and a fork boots parent rootfses
//! without materializing anything. Run logs and build notes live here, not in the world runtime.

pub(super) mod progress;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use slipfork_image::{
    BuildArgs, BuildRequest, Cache, ImageReference, NativeImageBuilder, PulledImage,
    planned_instruction_width,
};
use slipfork_world::{LogStore, WorldName, world_plan_with};

use super::Failure;
use super::image::{ImageLab, now_ms, pull_externals};
use crate::world::HostEnv;
use crate::world::WorldHeads;
use crate::world::features::WorldFeatures;
use crate::world::{HEADS_VERSION, ImageHead};
use progress::{AttemptFailure, ImageAttempt};

/// Compiles the world, seals every declared image (and resource image), and writes one head file.
///
/// The first error returns before any head file is written, so a half-built world is never booted.
pub(crate) fn build_world_heads(
    host: &HostEnv,
    project: &Path,
    world: &WorldName,
    no_cache: bool,
    emit: &mut dyn FnMut(Value),
) -> Result<WorldHeads, Failure> {
    let path = project.join("Forkfile");
    let source = fs::read_to_string(&path).map_err(|error| {
        Failure::new(
            "forkfile",
            format!(
                "cannot read `{}`: {error}\nnext: create a Forkfile, then retry",
                path.display()
            ),
        )
    })?;
    let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
        let diagnostic = diagnostics.first();
        Failure::new(
            "invalid-forkfile",
            format!(
                "Forkfile:{}: {}",
                diagnostic.map_or(0, |item| item.span.line()),
                diagnostic.map_or("the Forkfile is invalid", |item| item.message.as_str())
            ) + "\nnext: run `slipfork fmt` after fixing the first error",
        )
    })?;
    let doc = *checked.document;
    let features = WorldFeatures::load_resolved(project, &doc).map_err(|message| {
        Failure::new(
            "feature",
            format!("{message}\nnext: {}", WorldFeatures::REMEDY),
        )
    })?;
    let plan = world_plan_with(&doc, project, &features).map_err(|error| {
        Failure::new(
            "forkfile-plan",
            format!("{error}\nnext: fix the Forkfile, then run `slipfork doctor`"),
        )
    })?;
    let image_inputs = crate::world::image_inputs(project, &doc, &plan.images, &features);
    let lab = ImageLab::for_project(host, project)?;
    let store = LogStore::at(host.store_root_for(project), world.clone());
    let puller = lab.puller(host);
    let step_host = lab.step_host(host)?;
    let builder = NativeImageBuilder::new(
        Arc::clone(&lab.engine),
        lab.puller(host).sharing_flights(&puller),
        lab.store.clone(),
        lab.architecture,
        step_host,
    )
    .with_cache(if no_cache { Cache::Cold } else { Cache::Warm });
    let mut images = BTreeMap::new();
    for node in &plan.images {
        let command = node
            .from
            .clone()
            .unwrap_or_else(|| node.paths(project).dockerfile.display().to_string());
        let started = Instant::now();
        let mut attempt = ImageAttempt::begin(emit, &plan, &store, project, &node.id, &command)?;
        let outcome = build_image(
            project,
            &doc,
            &features,
            world,
            node,
            &puller,
            &builder,
            &mut attempt,
        );
        let elapsed = millis(started);
        match outcome {
            Ok((head, state, last)) => {
                images.insert(node.id.clone(), head);
                attempt.finish(&state, elapsed, last.as_ref());
            }
            Err(failure) => return Err(attempt.fail(failure, elapsed)),
        }
    }
    // Resource images (`postgres:18`, `redis:7`) are pulled by the daemon too: the supervisor
    // boots every pod from a head and never constructs a puller. Collect first so the loop can
    // insert into `images` while it iterates the plan's immutable resource list.
    let missing: Vec<String> = plan
        .resources
        .iter()
        .filter_map(|resource| resource.resource.image())
        .filter(|reference| !images.contains_key(*reference))
        .map(str::to_owned)
        .collect();
    for reference in missing {
        let pulled = pull_reference(&puller, &reference).map_err(|message| {
            Failure::new(
                "registry-pull",
                format!(
                    "pull `{reference}` failed: {message}\nnext: check the reference and registry credentials, then retry"
                ),
            )
        })?;
        images.insert(reference, pulled_head(pulled));
    }
    let heads = WorldHeads {
        v: HEADS_VERSION,
        world: world.as_str().to_owned(),
        image_inputs,
        built_at_ms: now_ms(),
        images,
    };
    heads
        .save(&WorldHeads::path(&host.runtime_dir, world))
        .map_err(|error| {
            Failure::new(
                "image-heads",
                format!("{error}\nnext: run `slipfork up` again"),
            )
        })?;
    Ok(heads)
}

/// Seals one declared image: a `from:` pull or a native Dockerfile build.
#[allow(clippy::too_many_arguments)]
fn build_image(
    project: &Path,
    doc: &slipfork_forkfile::ast::Forkfile,
    features: &slipfork_forkfile::FeatureSet,
    world: &WorldName,
    node: &slipfork_world::ImageNode,
    puller: &slipfork_image::OciPuller,
    builder: &NativeImageBuilder,
    attempt: &mut ImageAttempt<'_>,
) -> Result<(ImageHead, String, Option<slipfork_image::StepTrace>), AttemptFailure> {
    if let Some(reference) = &node.from {
        let pulled = pull_row(puller, reference, attempt)?;
        let state = if pulled.cached { "cached" } else { "pulled" }.to_owned();
        return Ok((pulled_head(pulled), state, None));
    }
    let paths = node.paths(project);
    let source = fs::read_to_string(&paths.dockerfile).map_err(|error| {
        AttemptFailure::bare(format!(
            "build `{}` failed: cannot read `{}`: {error}\nnext: create the Dockerfile named by this image, then retry",
            node.id,
            paths.dockerfile.display()
        ))
    })?;
    let mut args = BuildArgs::new();
    for (key, value) in doc.eval_image_args_with(world.as_str(), "http", &node.id, features) {
        args.declare(key, value);
    }
    let target = doc.eval_image_stage_with(&node.id, features);
    let request = BuildRequest {
        context: paths.context,
        dockerfile: paths.dockerfile,
        args,
        target: target.clone(),
        externals: BTreeMap::new(),
    };
    let width = planned_instruction_width(&source, target.as_deref()).unwrap_or(0);
    let report = attempt
        .events(|log| {
            log.plan_width(width);
            let externals = pull_externals(&source, target.as_deref(), puller, log)?;
            builder.build(
                &BuildRequest {
                    externals,
                    ..request.clone()
                },
                log,
                now_ms(),
            )
        })
        .map_err(|error| {
            AttemptFailure::engine(
                format!(
                    "build `{}` failed: {}\nnext: fix the failing Dockerfile instruction and retry, or run `slipfork logs`",
                    node.id,
                    error.cause()
                ),
                &error,
                None,
            )
        })?;
    let last = report.last;
    let state = if report.cache_misses == 0 {
        "cached"
    } else {
        "built"
    }
    .to_owned();
    let head = ImageHead {
        rootfs: report.rootfs,
        inherited_env: report
            .config
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        argv: report.config.exec_argv(),
        workdir: report.config.workdir,
        state: state.clone(),
    };
    Ok((head, state, last))
}

/// Pulls one `FROM`/resource reference with its own live `FROM` row.
fn pull_row(
    puller: &slipfork_image::OciPuller,
    reference: &str,
    attempt: &mut ImageAttempt<'_>,
) -> Result<PulledImage, AttemptFailure> {
    let parsed = ImageReference::parse(reference).map_err(|error| {
        AttemptFailure::engine(
            format!("pull `{reference}` failed: {}", error.cause()),
            &error,
            None,
        )
    })?;
    attempt
        .events(|log| {
            log.announce("FROM", reference);
            let started = Instant::now();
            match puller.pull_into(&parsed, log) {
                Ok(image) => {
                    log.done(started.elapsed());
                    Ok(image)
                }
                Err(error) => {
                    log.failed(started.elapsed(), error.exit_code());
                    Err(error)
                }
            }
        })
        .map_err(|error| {
            AttemptFailure::engine(
                format!("pull `{reference}` failed: {}", error.cause()),
                &error,
                None,
            )
        })
}

/// Pulls one reference with no image row (a resource included outside the plan's image list).
fn pull_reference(
    puller: &slipfork_image::OciPuller,
    reference: &str,
) -> Result<PulledImage, String> {
    let parsed = ImageReference::parse(reference).map_err(|error| error.cause().to_owned())?;
    puller
        .pull_into(
            &parsed,
            &mut slipfork_image::StepLog::with_events(&mut |_| {}),
        )
        .map_err(|error| error.cause().to_owned())
}

/// The head one pulled image contributes: sealed rootfs, inherited env, and entrypoint+cmd.
fn pulled_head(image: PulledImage) -> ImageHead {
    let state = if image.cached { "cached" } else { "pulled" }.to_owned();
    let mut argv = image.facts.entrypoint;
    argv.extend(image.facts.cmd);
    ImageHead {
        rootfs: image.rootfs,
        inherited_env: image.facts.env,
        argv,
        workdir: "/".to_owned(),
        state,
    }
}

/// Whole milliseconds from one attempt's start.
fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl super::Daemon {
    /// Resolves one `up`/`restart` request's project, world, and `--no-cache` flag.
    pub(super) fn up_target(&self, params: &Value) -> Result<(PathBuf, WorldName, bool), Failure> {
        let project = super::lifecycle::up::project_path(params)?;
        let requested = params.get("world").and_then(Value::as_str);
        let world = crate::world::world_name(&project, requested)
            .map_err(|message| Failure::new("invalid-world", message))?;
        let no_cache = params
            .get("no_cache")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((project, world, no_cache))
    }

    /// Restarts the live pods, or reconciles and reboots when the recorded head changed.
    pub(super) fn dispatch_restart(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Option<Value>, Failure> {
        // Drop the core guard before the reconcile arm re-locks it: a temporary `MutexGuard` in a
        // `match` scrutinee lives for the whole arm and would deadlock on the second `core()`.
        let current = super::lifecycle::up::fingerprint_for(params)?;
        let action = self.core()?.restart(params, current, emit)?;
        match action {
            super::lifecycle::core::RestartAction::Restarted(value) => Ok(Some(value)),
            super::lifecycle::core::RestartAction::Reconcile => {
                let (project, world, no_cache) = self.up_target(params)?;
                self.preplan(&project, &world, no_cache, emit)?;
                let plan = self
                    .core()?
                    .detach_live(&super::rpc::key_of(&project))
                    .ok_or_else(|| {
                        Failure::new("no-world", "no world is running in this directory")
                    })?;
                super::lifecycle::up::reconcile(plan);
                let mut session = self.core()?.begin_up(params)?;
                session.plan_already_announced();
                let mut reply = session.run(self, emit)?;
                // The boot stream already printed `✓ Ready`; mark the reply so the CLI never
                // fabricates a `Restarted (0 pods)` line for a full rebuild.
                if let Some(value) = reply.as_object_mut() {
                    value.insert("reconciled".to_owned(), Value::Bool(true));
                }
                Ok(Some(reply))
            }
        }
    }

    /// Announces the world plan once, then seals every image head before the old world stops.
    pub(super) fn preplan(
        &self,
        project: &Path,
        world: &WorldName,
        no_cache: bool,
        emit: &mut dyn FnMut(Value),
    ) -> Result<(), Failure> {
        let rows = super::lifecycle::plan::world_rows(project);
        super::lifecycle::plan::emit(&rows, rows.len(), emit);
        let host = self.core()?.host().clone();
        build_world_heads(&host, project, world, no_cache, emit).map(|_| ())
    }
}
