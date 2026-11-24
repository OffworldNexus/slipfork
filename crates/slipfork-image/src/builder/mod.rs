//! Native multi-stage `Dockerfile` evaluation into sealed Btrfs step subvolumes.

mod run;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use slipfork_world::WorldEngine;

use crate::Architecture;
use crate::BaseImageBuilder;
use crate::digest::hex;
use crate::dockerfile::{
    BuildArgs, Command, CopySpec, DockerfileParser, ImageConfig, Instruction, Stage, StageBase,
    Variables,
};
use crate::error::{Error, Result};
use crate::puller::{OciPuller, PulledImage};
use crate::reference::ImageReference;
use crate::runtime::{CrunStepRuntime, StepHost, StepLog};
use crate::steps::{StepCache, StepLayout, StepRecord};
use run::BuildRun;

/// Everything one `build` call needs; every path is absolute and caller-injected.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    /// Directory the `Dockerfile` reads from (`COPY`/`ADD` sources).
    pub context: PathBuf,
    /// The `Dockerfile` itself (usually `<context>/Dockerfile`).
    pub dockerfile: PathBuf,
    /// Non-secret, compiler-validated `arg` values.
    pub args: BuildArgs,
    /// Stop after this `AS` stage instead of building the final stage.
    pub target: Option<String>,
}

/// The last instruction a build applied, named exactly as the author wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepTrace {
    /// Instruction keyword (`run`, `copy`, …).
    pub phase: &'static str,
    /// Command text without the keyword, as written in the `Dockerfile`.
    pub command: String,
    /// Source line in the `Dockerfile`.
    pub line: usize,
}

/// One finished image plus cache statistics.
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// Sealed rootfs of the target stage.
    pub rootfs: PathBuf,
    /// Cache key of the final step.
    pub key: String,
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

/// Native `Dockerfile` builder behind the [`BaseImageBuilder`] port.
pub struct NativeImageBuilder {
    world: Arc<dyn WorldEngine>,
    puller: OciPuller,
    layout: StepLayout,
    architecture: Architecture,
    host: StepHost,
    cache: StepCache,
}

/// One stage's current sealed rootfs plus metadata.
#[derive(Debug, Clone)]
struct StageState {
    rootfs: PathBuf,
    key: String,
    config: ImageConfig,
}

impl NativeImageBuilder {
    /// Builds the builder from injected roots, the OCI puller, and host facts.
    #[must_use]
    pub fn new(
        world: Arc<dyn WorldEngine>,
        puller: OciPuller,
        layout: StepLayout,
        architecture: Architecture,
        host: StepHost,
    ) -> Self {
        let cache = StepCache::new(Arc::clone(&world), layout.clone());
        Self {
            world,
            puller,
            layout,
            architecture,
            host,
            cache,
        }
    }

    /// The architecture every `FROM <image>` is resolved for.
    #[must_use]
    pub const fn architecture(&self) -> Architecture {
        self.architecture
    }

    /// Evaluates `request`, streaming `RUN` output to `log` and reusing sealed step subvolumes.
    pub fn build(&self, request: &BuildRequest, log: &mut StepLog<'_>) -> Result<BuildReport> {
        let started = Instant::now();
        let source = fs::read_to_string(&request.dockerfile)
            .map_err(|source| Error::io("read", &request.dockerfile, source))?;
        let stages = DockerfileParser::parse(&source)?;
        let walk = stage_walk(&stages, request.target.as_deref())?;
        // Only the requested prefix runs, so a stage past `--target` must never force `crun` or
        // pull context files into this build.
        let walked = &stages[..=walk.end];
        let runtime = CrunStepRuntime::new(self.host.clone());
        if walked
            .iter()
            .flat_map(|stage| &stage.instructions)
            .any(|parsed| matches!(parsed.instruction, Instruction::Run(_)))
        {
            runtime.ensure_crun()?;
        }
        self.layout.ensure()?;
        let mut run = BuildRun::new(self, request, &runtime, log, walked);
        let state = self.build_stages(&mut run, &stages, walk.end)?;
        Ok(BuildReport {
            rootfs: state.rootfs,
            key: state.key,
            config: state.config,
            cache_hits: run.cache_hits(),
            cache_misses: run.cache_misses(),
            build_time: started.elapsed(),
            last: run.last_instruction(),
        })
    }

    /// Evaluates the stages a [`stage_walk`] selected and returns the walked stage's state.
    fn build_stages(
        &self,
        run: &mut BuildRun<'_, '_, '_>,
        stages: &[Stage],
        end: usize,
    ) -> Result<StageState> {
        for stage in &stages[..end] {
            let state = run.stage(stage)?;
            run.finish_stage(stage, state);
        }
        // `stage_walk` proved `end < stages.len()`, so the target stage always exists.
        run.stage(&stages[end])
    }

    fn base_state(
        &self,
        stage: &Stage,
        built: &[(Option<String>, StageState)],
        log: &mut StepLog<'_>,
    ) -> Result<StageState> {
        match &stage.base {
            StageBase::Image(reference) => {
                let reference = ImageReference::parse(reference)?;
                let image = self.puller.pull_into(&reference, log)?;
                Ok(StageState {
                    key: format!("base-{}", image.digest),
                    config: ImageConfig::from(&image.facts),
                    rootfs: image.rootfs,
                })
            }
            StageBase::Stage(name) => built
                .iter()
                .find(|(alias, _)| alias.as_deref() == Some(name.as_str()))
                .map(|(_, state)| state.clone())
                .ok_or_else(|| Error::UnknownStage { name: name.clone() }),
        }
    }

    fn source_state(
        &self,
        from: &str,
        built: &[(Option<String>, StageState)],
        stages: &[Stage],
    ) -> Result<StageState> {
        if let Some((_, state)) = built
            .iter()
            .find(|(alias, _)| alias.as_deref() == Some(from))
        {
            return Ok(state.clone());
        }
        if let Ok(index) = from.parse::<usize>() {
            if let Some((_, state)) = built.get(index) {
                return Ok(state.clone());
            }
            if let Some(alias) = stages.get(index).and_then(|stage| stage.alias.as_ref())
                && let Some((_, state)) = built
                    .iter()
                    .find(|(built_alias, _)| built_alias.as_deref() == Some(alias.as_str()))
            {
                return Ok(state.clone());
            }
        }
        Err(Error::UnknownStage {
            name: from.to_owned(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn filesystem_step<F>(
        &self,
        parent: &StageState,
        instruction: &Instruction,
        line: usize,
        args: &BTreeMap<String, String>,
        context_hash: &str,
        config: &ImageConfig,
        apply: F,
    ) -> Result<(StageState, bool)>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let label = instruction.render();
        let key = state_key(instruction, args, context_hash, config, &parent.key);
        if let Some(sealed) = self.cache.lookup(&key)? {
            return Ok((
                StageState {
                    rootfs: sealed,
                    key,
                    config: config.clone(),
                },
                true,
            ));
        }
        let writer = self.layout.writer(&key);
        self.cache.abandon(&writer);
        self.world.clone_subvolume(&parent.rootfs, &writer)?;
        if let Err(err) = apply(&writer) {
            self.cache.abandon(&writer);
            return Err(err);
        }
        let rootfs = self
            .cache
            .store(
                &key,
                &writer,
                &StepRecord {
                    key: key.clone(),
                    parent: parent.key.clone(),
                    instruction: label,
                    line,
                },
            )
            .map_err(|err| at_line(err, line, instruction))?;
        Ok((
            StageState {
                rootfs,
                key,
                config: config.clone(),
            },
            false,
        ))
    }
}

impl BaseImageBuilder for NativeImageBuilder {
    fn pull(&self, reference: &ImageReference) -> Result<PulledImage> {
        self.puller.pull(reference)
    }
}

fn state_key(
    instruction: &Instruction,
    args: &BTreeMap<String, String>,
    context_hash: &str,
    config: &ImageConfig,
    parent: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"slipfork-step-v1\0");
    hasher.update(parent.as_bytes());
    hasher.update(b"\0");
    hasher.update(instruction.render().as_bytes());
    hasher.update(b"\0");
    for (key, value) in args {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(context_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(config.fingerprint().as_bytes());
    hex(&hasher.finalize())
}

fn expand_spec(spec: &CopySpec, variables: &Variables) -> CopySpec {
    CopySpec {
        from: spec.from.clone(),
        chown: spec.chown,
        sources: spec
            .sources
            .iter()
            .map(|source| variables.expand(source))
            .collect(),
        dest: variables.expand(&spec.dest),
    }
}

fn expand_command(command: &Command, variables: &Variables) -> Command {
    match command {
        Command::Shell(script) => Command::Shell(variables.expand(script)),
        Command::Exec(argv) => {
            Command::Exec(argv.iter().map(|arg| variables.expand(arg)).collect())
        }
    }
}

/// Folds a step failure into a line-decorated `StepFailed` naming the instruction as written.
fn at_line(err: Error, line: usize, instruction: &Instruction) -> Error {
    let phase = instruction.phase();
    let command = instruction.command_text();
    match err {
        Error::Dockerfile { .. } => err,
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
            next: "declare it above this stage with `FROM <image> AS <name>`".to_owned(),
        },
        other => other,
    }
}

/// Drops the internal `images/steps/<key>` prefix so errors name container paths only.
fn visible_path(path: &Path) -> String {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    match components.iter().position(|component| component == "steps") {
        Some(index) if components.len() > index + 2 => {
            format!("/{}", components[index + 2..].join("/"))
        }
        _ => path.display().to_string(),
    }
}
