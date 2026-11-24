//! Per-instruction evaluation of one build walk: metadata, `COPY`/`ADD`, and `RUN`.

use std::collections::BTreeMap;
use std::fs;

use super::{
    BuildRequest, NativeImageBuilder, Stage, StageState, at_line, expand_command, expand_spec,
    state_key,
};
use crate::context::BuildContext;
use crate::copy;
use crate::dockerfile::{CopySpec, Instruction, ParsedInstruction, RunSpec, Variables};
use crate::error::{Error, Result};
use crate::runtime::{CrunStepRuntime, StepLog};

/// Mutable state of one `build` walk: stage outputs, cache counters, and the log sink.
pub(super) struct BuildRun<'a, 's, 'l> {
    builder: &'a NativeImageBuilder,
    request: &'a BuildRequest,
    context: &'a BuildContext,
    context_hash: &'a str,
    runtime: &'a CrunStepRuntime,
    log: &'l mut StepLog<'s>,
    stages: &'a [Stage],
    built: Vec<(Option<String>, StageState)>,
    hits: usize,
    misses: usize,
}

impl<'a, 's, 'l> BuildRun<'a, 's, 'l> {
    /// Bundles the injected roots and the mutable log sink for one build walk.
    pub(super) fn new(
        builder: &'a NativeImageBuilder,
        request: &'a BuildRequest,
        context: &'a BuildContext,
        context_hash: &'a str,
        runtime: &'a CrunStepRuntime,
        log: &'l mut StepLog<'s>,
        stages: &'a [Stage],
    ) -> Self {
        Self {
            builder,
            request,
            context,
            context_hash,
            runtime,
            log,
            stages,
            built: Vec::new(),
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

    /// Records a finished stage so later `COPY --from` and `FROM <stage>` can reuse it.
    pub(super) fn finish_stage(&mut self, stage: &Stage, state: StageState) {
        self.built.push((stage.alias.clone(), state));
    }

    /// Applies every instruction of `stage`, returning its sealed rootfs and metadata.
    pub(super) fn stage(&mut self, stage: &Stage) -> Result<StageState> {
        let mut state = self.builder.base_state(stage, &self.built)?;
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

    /// Applies one instruction, folding every failure into a line-decorated `StepFailed`.
    fn apply(
        &mut self,
        state: &mut StageState,
        args: &mut BTreeMap<String, String>,
        parsed: &ParsedInstruction,
    ) -> Result<Option<bool>> {
        let label = parsed.instruction.render();
        self.apply_inner(state, args, parsed)
            .map_err(|err| at_line(err, parsed.line, &label))
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
                let (next, hit) = self.builder.filesystem_step(
                    state,
                    &parsed.instruction,
                    line,
                    args,
                    self.context_hash,
                    &next_config,
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
        let variables = Variables::new(&self.request.args, &state.config.env);
        let expanded = expand_spec(spec, &variables);
        let is_add = matches!(&parsed.instruction, Instruction::Add(_));
        let owner = expanded.chown.unwrap_or_else(|| self.builder.host.owner());
        let from_state = match &expanded.from {
            Some(from) => Some(self.builder.source_state(from, &self.built, self.stages)?),
            None => None,
        };
        let instruction = if is_add {
            Instruction::Add(expanded.clone())
        } else {
            Instruction::Copy(expanded.clone())
        };
        let config = state.config.clone();
        let (next, hit) = self.builder.filesystem_step(
            state,
            &instruction,
            parsed.line,
            args,
            self.context_hash,
            &config,
            |writer| {
                let destination = copy::Destination::new(writer, &config.workdir);
                match &from_state {
                    Some(source) => copy::copy_rootfs(
                        &source.rootfs,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        owner,
                    ),
                    None if is_add => copy::add_context(
                        self.context,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        owner,
                    ),
                    None => copy::copy_context(
                        self.context,
                        &expanded.sources,
                        &expanded.dest,
                        destination,
                        owner,
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
        let variables = Variables::new(&self.request.args, &state.config.env);
        let run = RunSpec {
            mounts: run.mounts.clone(),
            command: expand_command(&run.command, &variables),
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
        let id = format!(
            "sf-{}-{}",
            std::process::id(),
            &state_key(&instruction, args, self.context_hash, &config, &state.key)[..16]
        );
        let runtime = self.runtime;
        let log = &mut *self.log;
        let (next, hit) = self.builder.filesystem_step(
            state,
            &instruction,
            line,
            args,
            self.context_hash,
            &config,
            |writer| runtime.run(&id, writer, &config, &run, &mounts, line, log),
        )?;
        *state = next;
        Ok(hit)
    }
}
