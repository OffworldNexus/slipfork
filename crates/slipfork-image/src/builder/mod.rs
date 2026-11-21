//! Native multi-stage `Dockerfile` evaluation into sealed Btrfs step subvolumes.

mod run;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use slipfork_world::WorldEngine;

use crate::Architecture;
use crate::BaseImageBuilder;
use crate::context::BuildContext;
use crate::digest::hex;
use crate::dockerfile::{
    BuildArgs, Command, CopySpec, DockerfileParser, ImageConfig, Instruction, Stage, StageBase,
    Variables,
};
use crate::error::{Error, Result};
use crate::puller::{OciPuller, PulledImage};
use crate::reference::ImageReference;
use crate::runtime::{CrunStepRuntime, StepHost};
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
    pub fn build(&self, request: &BuildRequest, log: &mut dyn Write) -> Result<BuildReport> {
        let started = Instant::now();
        let source = fs::read_to_string(&request.dockerfile)
            .map_err(|source| Error::io("read", &request.dockerfile, source))?;
        let stages = DockerfileParser::parse(&source)?;
        let runtime = CrunStepRuntime::new(self.host.clone());
        if stages
            .iter()
            .flat_map(|stage| &stage.instructions)
            .any(|parsed| matches!(parsed.instruction, Instruction::Run(_)))
        {
            runtime.ensure_crun()?;
        }
        let context = BuildContext::load(&request.context)?;
        let context_hash = context.hash()?;
        self.layout.ensure()?;
        let target = request
            .target
            .as_ref()
            .map(|name| name.to_ascii_lowercase());
        if let Some(target) = &target
            && !stages
                .iter()
                .any(|stage| stage.alias.as_deref() == Some(target.as_str()))
        {
            return Err(Error::UnknownStage {
                name: target.clone(),
            });
        }
        let mut run = BuildRun::new(
            self,
            request,
            &context,
            &context_hash,
            &runtime,
            log,
            &stages,
        );
        let state = self.build_stages(&mut run, &stages, target.as_deref())?;
        Ok(BuildReport {
            rootfs: state.rootfs,
            key: state.key,
            config: state.config,
            cache_hits: run.cache_hits(),
            cache_misses: run.cache_misses(),
            build_time: started.elapsed(),
        })
    }

    /// Walks every stage in declaration order and returns the requested (or final) stage state.
    fn build_stages(
        &self,
        run: &mut BuildRun<'_>,
        stages: &[Stage],
        target: Option<&str>,
    ) -> Result<StageState> {
        let mut final_state = None;
        for stage in stages {
            let state = run.stage(stage)?;
            if target.is_none_or(|target| stage.alias.as_deref() == Some(target)) {
                final_state = Some(state.clone());
            }
            run.finish_stage(stage, state);
        }
        final_state.ok_or_else(|| Error::UnknownStage {
            name: target.unwrap_or("final").to_owned(),
        })
    }

    fn base_state(
        &self,
        stage: &Stage,
        built: &[(Option<String>, StageState)],
    ) -> Result<StageState> {
        match &stage.base {
            StageBase::Image(reference) => {
                let reference = ImageReference::parse(reference)?;
                let image = self.puller.pull(&reference)?;
                Ok(StageState {
                    rootfs: image.rootfs,
                    key: format!("base-{}", image.digest),
                    config: ImageConfig::default(),
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
            .map_err(|err| at_line(err, line, &instruction.render()))?;
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

/// Folds a step failure into a line-decorated `StepFailed` with a user-facing path.
fn at_line(err: Error, line: usize, label: &str) -> Error {
    match err {
        Error::Dockerfile { .. } => err,
        Error::StepFailed {
            line: 0,
            instruction,
            message,
            next,
        } => Error::StepFailed {
            line,
            instruction: if instruction.is_empty() {
                label.to_owned()
            } else {
                instruction
            },
            message,
            next,
        },
        Error::Io {
            action,
            path,
            source,
        } => Error::StepFailed {
            line,
            instruction: label.to_owned(),
            message: format!("could not {action} `{}`: {source}", visible_path(&path)),
            next: "check the build context permissions and free disk space, then retry".to_owned(),
        },
        Error::CrunMissing { path } => Error::StepFailed {
            line,
            instruction: label.to_owned(),
            message: format!("`RUN` needs `crun`, but `{}` is missing", path.display()),
            next:
                "install crun (`apt install crun`, `dnf install crun`, `pacman -S crun`) and retry"
                    .to_owned(),
        },
        Error::UnknownStage { name } => Error::StepFailed {
            line,
            instruction: label.to_owned(),
            message: format!("no build stage named `{name}`"),
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
