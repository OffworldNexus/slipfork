//! Native multi-stage `Dockerfile` evaluation into sealed snapshot nodes.
//!
//! [`NativeImageBuilder::build`] is a thin orchestrator: it parses the `Dockerfile`, plans every
//! content key through [`BuildPlanner`], then asks the world's [`SnapshotStore`] to reconcile one
//! graph per stage. The planner never writes disk; the store owns sealing, reuse, and GC.

mod externals;
mod materialize;
mod rows;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slipfork_world::snapshot::{PhaseTiming, TailPhase};
use slipfork_world::{Cache, Node, Observed, SnapshotError, SnapshotStore, WorldEngine};

use crate::Architecture;
use crate::BaseImageBuilder;
use crate::cache_mounts::CacheMounts;
use crate::context::BuildContext;
use crate::dockerfile::{DockerfileParser, ImageConfig, Instruction, Stage};
use crate::error::{Error, Result};
use crate::inputs;
use crate::plan::{BuildPlanner, RefResolver, StepTask};
use crate::puller::{OciPuller, PulledImage};
use crate::reference::ImageReference;
use crate::runtime::{CrunStepRuntime, StepHost, StepLog};
use crate::step_log::StepPlace;

pub use crate::plan::StepTrace;
pub use externals::{ExternalFrom, ExternalImage, external_references};
pub(crate) use externals::{FromSource, classify_from};

use materialize::TaskMaterializer;
use rows::StageRows;

/// Everything one `build` call needs; every path is absolute and caller-injected.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    /// Directory the `Dockerfile` reads from (`COPY`/`ADD` sources).
    pub context: PathBuf,
    /// The `Dockerfile` itself (usually `<context>/Dockerfile`).
    pub dockerfile: PathBuf,
    /// Non-secret, compiler-validated `arg` values.
    pub args: crate::dockerfile::BuildArgs,
    /// Stop after this `AS` stage instead of building the final stage.
    pub target: Option<String>,
    /// Rootfs the composition root pulled for each `COPY --from=<image>` reference, keyed by the
    /// reference exactly as the `Dockerfile` wrote it.
    pub externals: BTreeMap<String, ExternalImage>,
}

/// One finished image plus cache statistics.
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// Sealed rootfs of the target stage.
    pub rootfs: PathBuf,
    /// Content key of the final materialized node.
    pub key: slipfork_world::NodeKey,
    /// Accumulated `ENV`/`WORKDIR`/`USER`/`EXPOSE`/`CMD`/`ENTRYPOINT` metadata.
    pub config: ImageConfig,
    /// Steps served from sealed subvolumes.
    pub cache_hits: usize,
    /// Steps cloned and applied in this run.
    pub cache_misses: usize,
    /// Wall time spent inside the builder.
    pub build_time: Duration,
    /// The last instruction this walk applied; `None` when the `Dockerfile` applied none.
    pub last: Option<StepTrace>,
    /// Manifest digest of the final stage's `FROM` image, when it has one.
    pub base_digest: Option<String>,
}

/// Declaration-ordered span of stages one build must evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageWalk {
    /// Index of the last stage to evaluate; its sealed state is the build report's result.
    pub end: usize,
}

/// Resolves real `--target` semantics: stages are evaluated in declaration order, the walk stops
/// after the requested stage, and a targetless build walks through the final stage.
pub fn stage_walk(stages: &[Stage], target: Option<&str>) -> Result<StageWalk> {
    let Some(target) = target else {
        return stages
            .len()
            .checked_sub(1)
            .map(|end| StageWalk { end })
            .ok_or_else(|| Error::UnknownStage {
                name: "final".to_owned(),
            });
    };
    let wanted = target.to_ascii_lowercase();
    stages
        .iter()
        .position(|stage| stage.alias.as_deref() == Some(wanted.as_str()))
        .map(|end| StageWalk { end })
        .ok_or_else(|| Error::UnknownStage {
            name: target.to_owned(),
        })
}

/// The widest instruction keyword in `stages`, so every step row in one image shares an args
/// column. `0` for an empty plan.
#[must_use]
pub fn instruction_width(stages: &[Stage]) -> usize {
    stages
        .iter()
        .flat_map(|stage| &stage.instructions)
        .map(|parsed| parsed.instruction.keyword().chars().count())
        .max()
        .unwrap_or(0)
}

/// The widest instruction keyword in the prefix `target` selects, using the build's own
/// `--target` walk so the caller never re-derives stage semantics.
pub fn planned_instruction_width(source: &str, target: Option<&str>) -> Result<usize> {
    let stages = DockerfileParser::parse(source)?;
    let walk = stage_walk(&stages, target)?;
    Ok(instruction_width(&stages[..=walk.end]))
}

/// Native `Dockerfile` builder behind the [`BaseImageBuilder`] port.
pub struct NativeImageBuilder {
    world: Arc<dyn WorldEngine>,
    puller: OciPuller,
    layout: slipfork_world::StoreLayout,
    architecture: Architecture,
    host: StepHost,
    cache: Cache,
}

impl NativeImageBuilder {
    /// Builds the builder from injected roots, the OCI puller, and host facts.
    #[must_use]
    pub fn new(
        world: Arc<dyn WorldEngine>,
        puller: OciPuller,
        layout: slipfork_world::StoreLayout,
        architecture: Architecture,
        host: StepHost,
    ) -> Self {
        Self {
            world,
            puller,
            layout,
            architecture,
            host,
            cache: Cache::Warm,
        }
    }

    /// Switches reconcile between reuse (`Warm`) and full re-execution (`Cold`).
    #[must_use]
    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    /// The architecture every `FROM <image>` is resolved for.
    #[must_use]
    pub const fn architecture(&self) -> Architecture {
        self.architecture
    }

    /// Evaluates `request`, streaming `RUN` output to `log` and reusing sealed snapshot nodes.
    ///
    /// `now_ms` is the caller's wall clock; the build never reads ambient time itself.
    pub fn build(
        &self,
        request: &BuildRequest,
        log: &mut StepLog<'_>,
        now_ms: u64,
    ) -> Result<BuildReport> {
        let started = Instant::now();
        let source = fs::read_to_string(&request.dockerfile)
            .map_err(|source| Error::io("read", &request.dockerfile, source))?;
        let stages = DockerfileParser::parse(&source)?;
        let walk = stage_walk(&stages, request.target.as_deref())?;
        let walked = &stages[..=walk.end];
        log.plan_width(instruction_width(walked));

        let runtime = CrunStepRuntime::new(self.host.clone());
        if walked
            .iter()
            .flat_map(|stage| &stage.instructions)
            .any(|parsed| matches!(parsed.instruction, Instruction::Run(_)))
        {
            runtime.ensure_crun()?;
        }
        let context = BuildContext::for_walk(&request.context, inputs::needs_context(walked))?;
        let resolver = RefResolver::new(
            &self.puller,
            self.layout.images_root(),
            self.cache == Cache::Cold,
        );
        let planned =
            BuildPlanner::new(request, walked, &context, &resolver, self.architecture).plan()?;

        let store = SnapshotStore::new(&self.layout, self.world.as_ref());
        let cache_mounts = CacheMounts::new(self.layout.images_root());
        let materializer = TaskMaterializer {
            world: self.world.as_ref(),
            puller: &self.puller,
            store: &store,
            context: &context,
            owner: self.host.owner(),
            runtime: &runtime,
            cache_mounts: &cache_mounts,
        };
        let log_cell = RefCell::new(log);
        let stash: RefCell<Option<Error>> = RefCell::new(None);
        let tasks = &planned.tasks;
        let mut counted: BTreeSet<slipfork_world::NodeKey> = BTreeSet::new();
        let mut hits = 0usize;
        let mut misses = 0usize;

        for stage in &planned.stages {
            debug_assert_eq!(stage.graph.head(), stage.head);
            let stage_keys: BTreeSet<slipfork_world::NodeKey> = stage.keyed_rows().collect();
            let mut rows = StageRows::default();
            let observer = |node: &Node, observed: Observed| {
                if !stage_keys.contains(&node.key) {
                    return;
                }
                let mut log = log_cell.borrow_mut();
                match observed {
                    Observed::Reused => rows.reused(stage, node.key, &mut log),
                    Observed::Tail { phase } => rows.tail(stage, node.key, phase, &mut log),
                    Observed::Building => rows.building(stage, node.key, &mut log),
                    Observed::Built { timing } => rows.built(node.key, timing, &mut log),
                    Observed::Failed { elapsed } => {
                        let code = stash.borrow().as_ref().and_then(Error::exit_code);
                        rows.failed(node.key, elapsed, code, &mut log);
                    }
                }
            };
            let outcome = store.reconcile_serial(
                &stage.graph,
                self.cache,
                now_ms,
                observer,
                |node, slot, _sink| {
                    let Some(task) = tasks.get(&node.key) else {
                        return Err(SnapshotError::Materialize(format!(
                            "no planned task for node {}",
                            node.key
                        )));
                    };
                    materializer
                        .apply(node, task, slot, &log_cell)
                        .map_err(|error| {
                            let message = error.cause();
                            *stash.borrow_mut() = Some(error);
                            SnapshotError::Materialize(message)
                        })
                },
            );
            match outcome {
                Ok(reconciled) => {
                    for key in &reconciled.built_keys {
                        if is_fs(tasks.get(key)) && counted.insert(*key) {
                            misses += 1;
                        }
                    }
                    for key in &reconciled.reused_keys {
                        if is_fs(tasks.get(key)) && counted.insert(*key) {
                            hits += 1;
                        }
                    }
                    let mut log = log_cell.borrow_mut();
                    stage.flush_metadata(rows.cursor, stage.rows.len(), &mut log);
                }
                Err(failure) => {
                    let code = stash.borrow().as_ref().and_then(Error::exit_code);
                    {
                        let mut log = log_cell.borrow_mut();
                        rows.close_open(Duration::ZERO, code, &mut log);
                    }
                    if let Some(error) = stash.into_inner() {
                        return Err(error);
                    }
                    return Err(Error::StepFailed {
                        line: usize::try_from(failure.line).unwrap_or(usize::MAX),
                        phase: "build",
                        instruction: failure.node.as_hex()[..12].to_owned().into(),
                        message: failure.cause.to_string(),
                        exit_code: None,
                        runtime: None,
                        next: "fix the failing Dockerfile step and re-run; sealed nodes are reused"
                            .to_owned(),
                    });
                }
            }
        }

        Ok(BuildReport {
            rootfs: store.node_dir(&planned.final_key),
            key: planned.final_key,
            config: planned.final_config,
            cache_hits: hits,
            cache_misses: misses,
            build_time: started.elapsed(),
            last: planned.last,
            base_digest: planned.base_digest,
        })
    }
}

impl BaseImageBuilder for NativeImageBuilder {
    fn pull(&self, reference: &ImageReference) -> Result<PulledImage> {
        self.puller.pull(reference)
    }
}

/// Whether `task` is the filesystem kind that counts toward cache hits/misses.
fn is_fs(task: Option<&StepTask>) -> bool {
    matches!(task, Some(StepTask::Fs { .. }))
}

/// The one `-v` note that proves where a settled step's seconds went.
fn phase_note(place: StepPlace, timing: PhaseTiming) -> String {
    let split = [
        (TailPhase::Cloning, timing.clone),
        (TailPhase::Executing, timing.exec),
        (TailPhase::Sealing, timing.seal),
        (TailPhase::Recording, timing.record),
        (TailPhase::Publishing, timing.publish),
    ]
    .iter()
    .map(|(phase, elapsed)| format!("{} {:.2}s", phase.as_str(), elapsed.as_secs_f64()))
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "step {}/{} {split} total {:.2}s",
        place.index,
        place.total,
        timing.total().as_secs_f64(),
    )
}

/// Folds a step failure into a line-decorated `StepFailed` naming the instruction as written.
pub(crate) fn at_line(err: Error, line: usize, instruction: &Instruction) -> Error {
    let phase = instruction.phase();
    let command = instruction.command_text();
    match err {
        Error::Dockerfile { .. } => err,
        unknown @ Error::UnknownAccount { .. } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: unknown.cause(),
            exit_code: None,
            runtime: None,
            next: unknown.remedy().unwrap_or_default(),
        },
        Error::StepFailed {
            message,
            exit_code,
            runtime,
            next,
            ..
        } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message,
            exit_code,
            runtime,
            next,
        },
        Error::Io {
            action,
            path,
            source,
        } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("could not {action} `{}`: {source}", visible_path(&path)),
            exit_code: None,
            runtime: None,
            next: "check the build context permissions and free disk space, then retry".to_owned(),
        },
        Error::Http { url, status } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("`{url}` returned HTTP {status}"),
            exit_code: None,
            runtime: None,
            next: "check the URL or the registry reference, then retry".to_owned(),
        },
        Error::Transport { url, message } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("could not reach `{url}`: {message}"),
            exit_code: None,
            runtime: None,
            next: "check the URL and your network connection, then retry".to_owned(),
        },
        Error::CrunMissing { path } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("`RUN` needs `crun`, but `{}` is missing", path.display()),
            exit_code: None,
            runtime: None,
            next:
                "install crun (`apt install crun`, `dnf install crun`, `pacman -S crun`) and retry"
                    .to_owned(),
        },
        Error::UnknownStage { name } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("no build stage named `{name}`"),
            exit_code: None,
            runtime: None,
            next:
                "declare it above this stage with `FROM <image> AS <name>`, or use a full image reference"
                    .to_owned(),
        },
        other => other,
    }
}

/// Drops the internal `snapshots/<slot>` prefix so errors name container paths only.
fn visible_path(path: &Path) -> String {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    match components
        .iter()
        .position(|component| component == "snapshots")
    {
        Some(index) if components.len() > index + 2 => {
            format!("/{}", components[index + 2..].join("/"))
        }
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{instruction_width, phase_note, planned_instruction_width};
    use crate::dockerfile::DockerfileParser;
    use crate::step_log::StepPlace;
    use slipfork_world::snapshot::PhaseTiming;
    use std::time::Duration;

    #[test]
    fn phase_note_reports_the_real_split_and_honest_total() {
        let timing = PhaseTiming {
            clone: Duration::ZERO,
            exec: Duration::from_secs_f64(3.02),
            seal: Duration::from_secs_f64(4.21),
            record: Duration::from_secs_f64(0.01),
            publish: Duration::ZERO,
        };
        assert_eq!(
            phase_note(StepPlace::new(6, 7, 0), timing),
            "step 6/7 clone 0.00s exec 3.02s seal 4.21s record 0.01s publish 0.00s total 7.24s"
        );
    }

    #[test]
    fn widest_keyword_spans_mixed_from_copy_add_run_workdir() {
        let source = "FROM python:3.14-trixie AS base\n\
                      COPY . /app\n\
                      ADD x /y\n\
                      RUN echo hi\n\
                      WORKDIR /code\n";
        let stages = DockerfileParser::parse(source).expect("the fixture parses");
        assert_eq!(instruction_width(&stages), "WORKDIR".len());
    }

    #[test]
    fn planned_width_honours_the_target_prefix() {
        let source = "FROM alpine AS base\n\
                      COPY . /app\n\
                      FROM alpine AS build\n\
                      RUN true\n\
                      FROM alpine AS later\n\
                      WORKDIR /code\n";
        assert_eq!(
            planned_instruction_width(source, Some("build")).expect("the target exists"),
            "COPY".len(),
            "a later stage's WORKDIR never widens the requested prefix"
        );
        assert_eq!(
            planned_instruction_width(source, None).expect("the final stage"),
            "WORKDIR".len()
        );
    }
}
