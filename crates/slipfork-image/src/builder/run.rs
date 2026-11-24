//! Per-instruction evaluation of one build walk: metadata, `COPY`/`ADD`, and `RUN`.

use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use super::{
    BuildRequest, NativeImageBuilder, Stage, StageState, StepTrace, at_line, expand_command,
    expand_spec, state_key,
};
use crate::context::BuildContext;
use crate::copy;
use crate::dockerfile::{
    CopyOptions, CopySpec, Instruction, ParsedInstruction, RunSpec, Variables,
};
use crate::error::{Error, Result};
use crate::identity::Identity;
use crate::runtime::{CrunStepRuntime, StepLog};
use crate::step_log::StepPlace;

/// Mutable state of one `build` walk: stage outputs, cache counters, and the live log sink.
pub(super) struct BuildRun<'a, 's, 'l> {
    builder: &'a NativeImageBuilder,
    request: &'a BuildRequest,
    runtime: &'a CrunStepRuntime,
    log: &'l mut StepLog<'s>,
    stages: &'a [Stage],
    context: Option<BuildContext>,
    context_hash: Option<String>,
    needs_context: bool,
    total: usize,
    index: usize,
    built: Vec<(Option<String>, StageState)>,
    last: Option<StepTrace>,
    hits: usize,
    misses: usize,
}

impl<'a, 's, 'l> BuildRun<'a, 's, 'l> {
    /// Bundles the injected roots and the mutable log sink for the walked stages of one build.
    pub(super) fn new(
        builder: &'a NativeImageBuilder,
        request: &'a BuildRequest,
        runtime: &'a CrunStepRuntime,
        log: &'l mut StepLog<'s>,
        stages: &'a [Stage],
    ) -> Self {
        // Every instruction is a live row: `FROM` per walked stage plus its instructions.
        let total = stages
            .iter()
            .map(|stage| 1 + stage.instructions.len())
            .sum();
        let needs_context = stages
            .iter()
            .flat_map(|stage| &stage.instructions)
            .any(|parsed| {
                matches!(
                    &parsed.instruction,
                    Instruction::Copy(spec) | Instruction::Add(spec) if spec.from.is_none()
                )
            });
        Self {
            builder,
            request,
            runtime,
            log,
            stages,
            context: None,
            context_hash: None,
            needs_context,
            total,
            index: 0,
            built: Vec::new(),
            last: None,
            hits: 0,
            misses: 0,
        }
    }

    /// Cache hits accumulated so far.
    pub(super) fn cache_hits(&self) -> usize {
        self.hits
    }

    /// Cache misses accumulated so far.
    pub(super) fn cache_misses(&self) -> usize {
        self.misses
    }

    /// The last instruction this walk applied, as the author wrote it.
    pub(super) fn last_instruction(&self) -> Option<StepTrace> {
        self.last.clone()
    }

    /// Records a finished stage so later `COPY --from` and `FROM <stage>` can reuse it.
    pub(super) fn finish_stage(&mut self, stage: &Stage, state: StageState) {
        self.built.push((stage.alias.clone(), state));
    }

    /// Applies every instruction of `stage`, returning its sealed rootfs and metadata.
    ///
    /// The `FROM` row is announced before the base pull and the one context walk, so neither a
    /// registry pull nor a large build context can leave the client without a live row.
    pub(super) fn stage(&mut self, stage: &Stage) -> Result<StageState> {
        let from = Instruction::From {
            base: stage.base.clone(),
            alias: stage.alias.clone(),
        };
        self.begin(&from, stage.line);
        let started = Instant::now();
        if let Err(error) = self.ensure_context() {
            self.log.failed(started.elapsed(), None);
            return Err(error);
        }
        let mut state = match self.builder.base_state(stage, &self.built, self.log) {
            Ok(state) => state,
            Err(error) => {
                self.log.failed(started.elapsed(), error.exit_code());
                return Err(error);
            }
        };
        self.log.done(started.elapsed());
        let mut local_args: BTreeMap<String, String> = self
            .request
            .args
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for parsed in &stage.instructions {
            if let Some(hit) = self.apply(&mut state, &mut local_args, parsed)? {
                if hit {
                    self.hits += 1;
                } else {
                    self.misses += 1;
                }
            }
        }
        Ok(state)
    }

    /// Applies one instruction, announcing it first and folding every failure into a
    /// line-decorated `StepFailed`.
    fn apply(
        &mut self,
        state: &mut StageState,
        args: &mut BTreeMap<String, String>,
        parsed: &ParsedInstruction,
    ) -> Result<Option<bool>> {
        self.begin(&parsed.instruction, parsed.line);
        let started = Instant::now();
        let outcome = self
            .apply_inner(state, args, parsed)
            .map_err(|err| at_line(err, parsed.line, &parsed.instruction));
        match &outcome {
            Ok(_) => self.log.done(started.elapsed()),
            Err(err) => self.log.failed(started.elapsed(), err.exit_code()),
        }
        let outcome = outcome?;
        self.last = Some(StepTrace {
            phase: parsed.instruction.phase(),
            command: parsed.instruction.command_text(),
            line: parsed.line,
        });
        Ok(outcome)
    }

    fn apply_inner(
        &mut self,
        state: &mut StageState,
        args: &mut BTreeMap<String, String>,
        parsed: &ParsedInstruction,
    ) -> Result<Option<bool>> {
        let line = parsed.line;
        match &parsed.instruction {
            Instruction::From { .. } => Ok(None),
            Instruction::Arg { name, default } => {
                let value = self
                    .request
                    .args
                    .get(name)
                    .map(str::to_owned)
                    .or_else(|| default.clone())
                    .unwrap_or_default();
                args.insert(name.clone(), value);
                Ok(None)
            }
            Instruction::Env(pairs) => {
                let variables = Variables::new(&self.request.args, &state.config.env);
                for (key, value) in pairs {
                    state
                        .config
                        .env
                        .insert(key.clone(), variables.expand(value));
                }
                Ok(None)
            }
            Instruction::User(_)
            | Instruction::Expose(_)
            | Instruction::Cmd(_)
            | Instruction::Entrypoint(_) => {
                state.config.apply(&parsed.instruction);
                Ok(None)
            }
            Instruction::Workdir(directory) => {
                let variables = Variables::new(&self.request.args, &state.config.env);
                let expanded = variables.expand(directory);
                let mut next_config = state.config.clone();
                let resolved = next_config.set_workdir(&expanded);
                let hash = self.context_hash.as_deref().unwrap_or_default();
                let (next, hit) = self.builder.filesystem_step(
                    state,
                    &parsed.instruction,
                    line,
                    args,
                    hash,
                    &next_config,
                    "",
                    |writer| {
                        let target = writer.join(resolved.trim_start_matches('/'));
                        fs::create_dir_all(&target)
                            .map_err(|source| Error::io("create", &target, source))
                    },
                )?;
                *state = next;
                Ok(Some(hit))
            }
            Instruction::Copy(spec) | Instruction::Add(spec) => {
                self.copy(state, args, parsed, spec).map(Some)
            }
            Instruction::Run(run) => self.run(state, args, line, run).map(Some),
        }
    }

    fn copy(
        &mut self,
        state: &mut StageState,
        args: &BTreeMap<String, String>,
        parsed: &ParsedInstruction,
        spec: &CopySpec,
    ) -> Result<bool> {
        let (expanded, is_add) = {
            let variables = Variables::new(&self.request.args, &state.config.env);
            let expanded = expand_spec(spec, &variables);
            let is_add = matches!(&parsed.instruction, Instruction::Add(_));
            (expanded, is_add)
        };
        // `--chown` names resolve against the destination stage's own rootfs, exactly as Docker
        // resolves them. A single-mapping build can only represent the invoking uid, so the write
        // is always host-owned and only an unknown account can fail the step.
        if let Some(account) = expanded.chown.as_deref() {
            Identity::resolve(&state.rootfs, Some(account))?;
        }
        let options = CopyOptions::new(self.builder.host.owner(), expanded.mode);
        let instruction = if is_add {
            Instruction::Add(expanded.clone())
        } else {
            Instruction::Copy(expanded.clone())
        };
        let config = state.config.clone();
        let Self {
            builder,
            request,
            context,
            context_hash,
            stages,
            built,
            ..
        } = self;
        let context = context
            .as_ref()
            .expect("the build context loads before COPY/ADD");
        let hash = context_hash.as_deref().unwrap_or_default();
        let from_state = match &expanded.from {
            Some(from) => Some(builder.source_state(from, built, stages, &request.externals)?),
            None => None,
        };
        let salt = from_state.as_ref().map_or("", |source| source.key.as_str());
        let (next, hit) = builder.filesystem_step(
            state,
            &instruction,
            parsed.line,
            args,
            hash,
            &config,
            salt,
            |writer| {
                let destination = copy::Destination::new(writer, &config.workdir);
                match &from_state {
                    Some(source) => copy::copy_rootfs(
                        &source.rootfs,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        options,
                    ),
                    None if is_add => copy::add_context(
                        context,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        options,
                    ),
                    None => copy::copy_context(
                        context,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        options,
                    ),
                }
            },
        )?;
        *state = next;
        Ok(hit)
    }

    fn run(
        &mut self,
        state: &mut StageState,
        args: &BTreeMap<String, String>,
        line: usize,
        run: &RunSpec,
    ) -> Result<bool> {
        let run = {
            let variables = Variables::new(&self.request.args, &state.config.env);
            RunSpec {
                mounts: run.mounts.clone(),
                command: expand_command(&run.command, &variables),
            }
        };
        let instruction = Instruction::Run(run.clone());
        let mounts: Vec<_> = run
            .mounts
            .iter()
            .map(|mount| {
                let id = mount.id.clone().unwrap_or_else(|| mount.target.clone());
                (mount.clone(), self.builder.layout.cache_mount(&id))
            })
            .collect();
        for (_, directory) in &mounts {
            fs::create_dir_all(directory)
                .map_err(|source| Error::io("create", directory, source))?;
        }
        let config = state.config.clone();
        let Self {
            builder,
            context_hash,
            runtime,
            log,
            ..
        } = self;
        let hash = context_hash.as_deref().unwrap_or_default();
        let id = format!(
            "sf-{}-{}",
            std::process::id(),
            &state_key(&instruction, args, hash, &config, &state.key, "")[..16]
        );
        let log = &mut **log;
        let (next, hit) = builder.filesystem_step(
            state,
            &instruction,
            line,
            args,
            hash,
            &config,
            "",
            |writer| runtime.run(&id, writer, &config, &run, &mounts, line, log),
        )?;
        *state = next;
        Ok(hit)
    }

    /// Loads and hashes the build context once, on the first `FROM`.
    fn ensure_context(&mut self) -> Result<()> {
        if self.context.is_some() {
            return Ok(());
        }
        let context = if self.needs_context {
            BuildContext::load(&self.request.context)?
        } else {
            BuildContext::empty(&self.request.context)
        };
        self.context_hash = Some(context.hash()?);
        self.context = Some(context);
        Ok(())
    }

    /// Announces the running instruction at its 1-based position.
    fn begin(&mut self, instruction: &Instruction, line: usize) {
        self.index += 1;
        self.log.started(
            StepPlace::new(self.index, self.total, line),
            instruction.keyword(),
            &instruction.command_text(),
        );
    }
}
