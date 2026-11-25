//! Expansion of one stage's instructions into content keys and materializer tasks.
//!
//! This module owns `ARG`/`ENV` folding, `COPY`/`ADD`/`RUN` expansion, and `--from` resolution;
//! the planner only schedules stages. Keeping the expansion here means no caller re-scans `$`
//! tokens or re-derives a `--from` classification downstream.

use std::collections::BTreeMap;

use slipfork_world::{Node, NodeKey, NodeKind};

use crate::Architecture;
use crate::builder::BuildRequest;
use crate::builder::{FromSource, classify_from};
use crate::context::BuildContext;
use crate::dockerfile::{Command, CopySpec, ImageConfig, Instruction, RunSpec, Stage, Variables};
use crate::error::{Error, Result};
use crate::plan::sources::{add_inputs, copy_inputs};
use crate::step_log::StepPlace;

use super::planner::{CopyOrigin, FsKind, PlannedRow, StepTask, line_u32};

/// One evaluated stage, remembered so a later `FROM <stage>` can reuse it.
#[derive(Debug)]
pub(crate) struct BuiltStage {
    /// `AS <alias>` as declared, matched case-insensitively.
    pub(crate) alias: Option<String>,
    /// The stage's last materialized key.
    pub(crate) key: NodeKey,
    /// Config a later `FROM <stage>` inherits.
    pub(crate) config: ImageConfig,
    /// Manifest digest of the stage's own `FROM`, when it has one.
    pub(crate) base_digest: Option<String>,
}

/// The build-wide state one stage folds into: its request, walked stages, and node maps.
pub(crate) struct StageScope<'a, 'm> {
    /// The build request (args and pre-pulled externals).
    pub(crate) request: &'a BuildRequest,
    /// The walked build context.
    pub(crate) context: &'a BuildContext,
    /// Every started stage, for `FROM <stage>` and `COPY --from`.
    pub(crate) stages: &'a [Stage],
    /// Stages already evaluated, in declaration order.
    pub(crate) built: &'a [BuiltStage],
    /// OCI architecture string every key is derived for.
    pub(crate) arch: &'a str,
    /// The build architecture, for `external` keys.
    pub(crate) architecture: Architecture,
    /// Build-wide node map this stage contributes to.
    pub(crate) nodes: &'m mut BTreeMap<NodeKey, Node>,
    /// Build-wide materializer task map.
    pub(crate) tasks: &'m mut BTreeMap<NodeKey, StepTask>,
}

/// One stage's fold: its live config, current key, rows, and expanded `ARG` scope.
pub(crate) struct StageFolder<'a, 'm> {
    scope: StageScope<'a, 'm>,
    rows: Vec<PlannedRow>,
    config: ImageConfig,
    current: NodeKey,
    args: BTreeMap<String, String>,
}

impl<'a, 'm> StageFolder<'a, 'm> {
    /// Opens one stage at its `FROM` key and config. `ARG` scope starts empty: only declarations
    /// before an instruction (Docker semantics) may enter that instruction's content key.
    pub(crate) fn new(scope: StageScope<'a, 'm>, config: ImageConfig, current: NodeKey) -> Self {
        Self {
            scope,
            rows: Vec::new(),
            config,
            current,
            args: BTreeMap::new(),
        }
    }

    /// The stage's live config, last key, and rows once every instruction is folded.
    pub(crate) fn into_parts(self) -> (ImageConfig, NodeKey, Vec<PlannedRow>) {
        (self.config, self.current, self.rows)
    }

    /// Folds one instruction into the live config, `ARG` scope, and the next content key.
    pub(crate) fn fold(&mut self, instruction: &Instruction, place: StepPlace) -> Result<()> {
        match instruction {
            Instruction::From { .. } => {}
            Instruction::Arg { name, default } => {
                let value = self
                    .scope
                    .request
                    .args
                    .get(name)
                    .map(str::to_owned)
                    .or_else(|| default.clone())
                    .or_else(|| self.args.get(name).cloned())
                    .unwrap_or_default();
                // The row repeats the value this ARG resolved to, never the raw `$` text.
                let row = Instruction::Arg {
                    name: name.clone(),
                    default: (!value.is_empty()).then(|| value.clone()),
                };
                self.args.insert(name.clone(), value);
                self.push_row(&row, None, place);
            }
            Instruction::Env(pairs) => {
                // Docker expands every pair against the pre-instruction snapshot, so the row
                // shows the resolved values even when a later pair references an earlier one.
                let variables = Variables::from_scope(&self.args, &self.config.env);
                let resolved: Vec<(String, String)> = pairs
                    .iter()
                    .map(|(key, value)| (key.clone(), variables.expand(value)))
                    .collect();
                for (key, value) in &resolved {
                    self.config.env.insert(key.clone(), value.clone());
                }
                self.push_row(&Instruction::Env(resolved), None, place);
            }
            Instruction::User(_)
            | Instruction::Expose(_)
            | Instruction::Cmd(_)
            | Instruction::Entrypoint(_) => {
                self.config.apply(instruction);
                self.push_row(instruction, None, place);
            }
            Instruction::Workdir(directory) => {
                let expanded = {
                    let variables = Variables::from_scope(&self.args, &self.config.env);
                    variables.expand(directory)
                };
                let mut next = self.config.clone();
                let resolved = next.set_workdir(&expanded);
                let expanded = Instruction::Workdir(resolved);
                let key = NodeKey::step_with_inputs(
                    self.scope.arch,
                    &[self.current],
                    &expanded.render(),
                    &next.fingerprint(),
                    &args_pairs(&self.args),
                    &[],
                );
                self.scope.nodes.insert(
                    key,
                    step_node(key, &[self.current], &expanded, line_u32(place.line)),
                );
                self.push_row(&expanded, Some(key), place);
                self.scope.tasks.insert(
                    key,
                    StepTask::Fs {
                        instruction: expanded,
                        config: next.clone(),
                        place,
                        kind: FsKind::Workdir,
                    },
                );
                self.current = key;
                self.config = next;
            }
            Instruction::Copy(spec) | Instruction::Add(spec) => {
                let is_add = matches!(instruction, Instruction::Add(_));
                let expanded = {
                    let variables = Variables::from_scope(&self.args, &self.config.env);
                    expand_spec(spec, &variables)
                };
                let origin = self
                    .origin(expanded.from.as_deref(), place.line)
                    .map_err(|error| crate::builder::at_line(error, place.line, instruction))?;
                let mut parents = vec![self.current];
                let mut extra = Vec::new();
                if let Some(origin) = &origin {
                    parents.push(origin.key());
                    extra.push((
                        "sources".to_owned(),
                        format!(
                            "from:{}:{}",
                            origin.key().as_hex(),
                            expanded.sources.join(",")
                        ),
                    ));
                } else if is_add {
                    extra.extend(add_inputs(self.scope.context, &expanded.sources)?);
                } else {
                    extra.extend(copy_inputs(self.scope.context, &expanded.sources)?);
                }
                let expanded = if is_add {
                    Instruction::Add(expanded)
                } else {
                    Instruction::Copy(expanded)
                };
                let key = NodeKey::step_with_inputs(
                    self.scope.arch,
                    &parents,
                    &expanded.render(),
                    &self.config.fingerprint(),
                    &args_pairs(&self.args),
                    &extra,
                );
                self.scope.nodes.insert(
                    key,
                    step_node(key, &parents, &expanded, line_u32(place.line)),
                );
                let kind = if is_add {
                    FsKind::Add(origin)
                } else {
                    FsKind::Copy(origin)
                };
                self.push_row(&expanded, Some(key), place);
                self.scope.tasks.insert(
                    key,
                    StepTask::Fs {
                        instruction: expanded,
                        config: self.config.clone(),
                        place,
                        kind,
                    },
                );
                self.current = key;
            }
            Instruction::Run(run) => {
                let expanded = {
                    let variables = Variables::from_scope(&self.args, &self.config.env);
                    RunSpec {
                        mounts: run.mounts.clone(),
                        command: expand_command(&run.command, &variables),
                    }
                };
                let expanded = Instruction::Run(expanded);
                let key = NodeKey::step_with_inputs(
                    self.scope.arch,
                    &[self.current],
                    &expanded.render(),
                    &self.config.fingerprint(),
                    &args_pairs(&self.args),
                    &[],
                );
                self.scope.nodes.insert(
                    key,
                    step_node(key, &[self.current], &expanded, line_u32(place.line)),
                );
                self.push_row(&expanded, Some(key), place);
                self.scope.tasks.insert(
                    key,
                    StepTask::Fs {
                        instruction: expanded,
                        config: self.config.clone(),
                        place,
                        kind: FsKind::Run,
                    },
                );
                self.current = key;
            }
        }
        Ok(())
    }

    fn push_row(&mut self, instruction: &Instruction, key: Option<NodeKey>, place: StepPlace) {
        self.rows.push(PlannedRow {
            place,
            keyword: instruction.keyword(),
            args: instruction.command_text(),
            key,
        });
    }

    /// Resolves `--from` against earlier stages or the pre-pulled externals.
    fn origin(&mut self, from: Option<&str>, line: usize) -> Result<Option<CopyOrigin>> {
        let Some(from) = from else {
            return Ok(None);
        };
        match classify_from(from, self.scope.stages) {
            Some(FromSource::Index(index) | FromSource::Stage(index)) => self
                .scope
                .built
                .get(index)
                .map(|stage| Some(CopyOrigin::Snapshot(stage.key)))
                .ok_or_else(|| Error::UnknownStage {
                    name: from.to_owned(),
                }),
            Some(FromSource::External) => {
                let (digest, rootfs) = {
                    let image = self.scope.request.externals.get(from).ok_or_else(|| {
                        Error::ExternalImageMissing {
                            reference: from.to_owned(),
                        }
                    })?;
                    (image.digest.clone(), image.rootfs.clone())
                };
                let key = NodeKey::external(self.scope.architecture.oci(), &digest);
                self.scope.nodes.entry(key).or_insert_with(|| Node {
                    key,
                    kind: NodeKind::External,
                    parents: Vec::new(),
                    canon: format!("COPY --from {from}"),
                    line: line_u32(line),
                });
                self.scope
                    .tasks
                    .entry(key)
                    .or_insert_with(|| StepTask::External {
                        rootfs: rootfs.clone(),
                    });
                Ok(Some(CopyOrigin::External { rootfs, key }))
            }
            None => Err(Error::UnknownStage {
                name: from.to_owned(),
            }),
        }
    }
}

fn step_node(key: NodeKey, parents: &[NodeKey], instruction: &Instruction, line: u32) -> Node {
    Node {
        key,
        kind: NodeKind::Step,
        parents: parents.to_vec(),
        canon: instruction.render(),
        line,
    }
}

fn args_pairs(args: &BTreeMap<String, String>) -> Vec<(String, String)> {
    args.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn expand_spec(spec: &CopySpec, variables: &Variables) -> CopySpec {
    CopySpec {
        from: spec.from.clone(),
        chown: spec.chown.clone(),
        mode: spec.mode,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(port: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("PORT".to_owned(), port.to_owned())])
    }

    /// `arg PORT = web.port` reaches the step cache: a changed value mints a new step key, so the
    /// image rebuilds; identical values keep the key, so it does not.
    #[test]
    fn a_changed_arg_value_rekeys_the_step_and_identical_args_stay_stable() {
        let parent = NodeKey::image(
            "amd64",
            &NodeKey::base("amd64", "sha256:parent", "linux/amd64"),
            "FROM app",
            "config",
        );
        let config = ImageConfig::default().fingerprint();
        let key = |args: &BTreeMap<String, String>| {
            NodeKey::step_with_inputs(
                "amd64",
                std::slice::from_ref(&parent),
                "RUN run-app",
                &config,
                &args_pairs(args),
                &[],
            )
        };
        let prod = key(&pairs("3000"));
        assert_ne!(
            prod,
            key(&pairs("5173")),
            "a changed `arg` value must rebuild the step"
        );
        assert_eq!(
            prod,
            key(&pairs("3000")),
            "identical args keep the step key"
        );
    }
}
