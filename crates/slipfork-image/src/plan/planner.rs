//! Pure `BuildPlanner`: per-stage graphs, content keys, and materializer tasks.
//!
//! The planner reads the walked stages once, resolves every `FROM` through the injected
//! [`RefResolver`], schedules one graph per stage, and returns immutable values. Instruction
//! expansion lives in [`super::expand`]; nothing here writes the store or executes a step.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use slipfork_world::{BuildGraph, Node, NodeKey, NodeKind};

use crate::Architecture;
use crate::builder::BuildRequest;
use crate::context::BuildContext;
use crate::dockerfile::{ImageConfig, Stage, StageBase};
use crate::error::{Error, Result};
use crate::plan::RefResolver;
use crate::reference::ImageReference;
use crate::step_log::{StepLog, StepPlace};

use super::expand::{BuiltStage, StageFolder, StageScope};

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

/// Where a `COPY --from`/`ADD --from` reads its tree.
#[derive(Debug, Clone)]
pub(crate) enum CopyOrigin {
    /// An earlier stage's last materialized node.
    Snapshot(NodeKey),
    /// A pre-pulled `images/oci` rootfs, addressable by its `external` key.
    External {
        /// Read-only rootfs the composition root pulled.
        rootfs: PathBuf,
        /// The `external` node key over that digest.
        key: NodeKey,
    },
}

impl CopyOrigin {
    /// The content key that names this source tree.
    pub(crate) const fn key(&self) -> NodeKey {
        match self {
            Self::Snapshot(key) | Self::External { key, .. } => *key,
        }
    }
}

/// What one filesystem step does inside its writable slot.
#[derive(Debug, Clone)]
pub(crate) enum FsKind {
    /// Execute through `crun`.
    Run,
    /// Create the current `WORKDIR`.
    Workdir,
    /// Copy from the build context or a `--from` tree.
    Copy(Option<CopyOrigin>),
    /// `ADD` from the build context, a `--from` tree, or a URL.
    Add(Option<CopyOrigin>),
}

/// One node's materialization recipe, keyed by its `NodeKey`.
#[derive(Debug, Clone)]
pub(crate) enum StepTask {
    /// A `FROM` rootfs; `scratch` creates nothing.
    Base {
        /// Reference exactly as the author wrote it (`scratch` included).
        reference: String,
        /// Manifest digest the plan sealed into this node's key; `None` only for `scratch`.
        digest: Option<String>,
    },
    /// A `COPY --from=<image>` rootfs already pulled under `images/oci`.
    External {
        /// Read-only pre-pulled rootfs.
        rootfs: PathBuf,
    },
    /// One filesystem instruction over the previous materialized node.
    Fs {
        /// Expanded instruction to apply.
        instruction: crate::dockerfile::Instruction,
        /// Config in effect for this instruction.
        config: ImageConfig,
        /// The live row this instruction occupies.
        place: StepPlace,
        /// How the step writes its slot.
        kind: FsKind,
    },
    /// An image's final config; record only, never a subvolume.
    Image {
        /// Final `ENV`/`WORKDIR`/`USER`/`EXPOSE`/`CMD`/`ENTRYPOINT`.
        config: ImageConfig,
    },
}

/// One live row in a stage's plan, in `Dockerfile` order.
#[derive(Debug, Clone)]
pub(crate) struct PlannedRow {
    /// Where the row sits in the walked build.
    pub(crate) place: StepPlace,
    /// Uppercase instruction keyword (`FROM`, `RUN`, `ENV`, …).
    pub(crate) keyword: &'static str,
    /// Arguments exactly as this row applies them.
    pub(crate) args: String,
    /// The node the row settles; `None` for metadata and inherited `FROM <stage>` rows.
    pub(crate) key: Option<NodeKey>,
}

/// One stage's graph plus its ordered live rows.
#[derive(Debug)]
pub(crate) struct PlannedStage {
    /// Every node needed by this stage (earlier stages included for `FROM <stage>`).
    pub(crate) graph: BuildGraph,
    /// The stage's `image` node.
    pub(crate) head: NodeKey,
    /// Every walked instruction row of this stage, in `Dockerfile` order.
    pub(crate) rows: Vec<PlannedRow>,
}

impl PlannedStage {
    /// The keys this stage's rows settle (base and filesystem nodes only).
    pub(crate) fn keyed_rows(&self) -> impl Iterator<Item = NodeKey> + '_ {
        self.rows.iter().filter_map(|row| row.key)
    }

    /// The first row settling `key` at or after `cursor`.
    pub(crate) fn row_index_from(&self, cursor: usize, key: NodeKey) -> Option<usize> {
        self.rows
            .get(cursor..)?
            .iter()
            .position(|row| row.key == Some(key))
            .map(|offset| cursor + offset)
    }

    /// The `(place, keyword, args)` one row publishes.
    pub(crate) fn row_parts(&self, index: usize) -> (StepPlace, &'static str, &str) {
        let row = &self.rows[index];
        (row.place, row.keyword, row.args.as_str())
    }

    /// Emits every metadata row in `[from, to)` as an immediate zero-time step.
    ///
    /// Node rows inside the span were already published as their node transition arrived; this
    /// only settles the metadata rows (`ENV`, `ARG`, `USER`, …) that never touch the store.
    pub(crate) fn flush_metadata(&self, from: usize, to: usize, log: &mut StepLog<'_>) {
        let end = to.min(self.rows.len());
        for row in self.rows.iter().take(end).skip(from) {
            if row.key.is_none() {
                log.started(row.place, row.keyword, &row.args);
                log.done(Duration::ZERO);
            }
        }
    }
}

/// The whole build: stages, materializer tasks, and the final image facts.
#[derive(Debug)]
pub(crate) struct PlannedBuild {
    /// Stages in declaration order; reconcile each graph before the next.
    pub(crate) stages: Vec<PlannedStage>,
    /// Every node's materializer recipe.
    pub(crate) tasks: BTreeMap<NodeKey, StepTask>,
    /// Final stage config.
    pub(crate) final_config: ImageConfig,
    /// Final stage's last materialized key (base or step).
    pub(crate) final_key: NodeKey,
    /// Digest of the final stage's `FROM` image, when it has one.
    pub(crate) base_digest: Option<String>,
    /// The last instruction the walk applied.
    pub(crate) last: Option<StepTrace>,
}

/// Everything one stage's opening `FROM` needs to resolve.
struct StageStart<'s> {
    stage: &'s Stage,
    arch: &'s str,
    platform: &'s str,
    place: StepPlace,
    built: &'s [BuiltStage],
}

/// A value-only planner over one walked `Dockerfile` prefix.
pub(crate) struct BuildPlanner<'a> {
    request: &'a BuildRequest,
    stages: &'a [Stage],
    context: &'a BuildContext,
    resolver: &'a RefResolver<'a>,
    architecture: Architecture,
}

impl<'a> BuildPlanner<'a> {
    /// Binds the planner to one request, its walked stages, context, and resolver.
    pub(crate) fn new(
        request: &'a BuildRequest,
        stages: &'a [Stage],
        context: &'a BuildContext,
        resolver: &'a RefResolver<'a>,
        architecture: Architecture,
    ) -> Self {
        Self {
            request,
            stages,
            context,
            resolver,
            architecture,
        }
    }

    /// Turns every walked stage into a graph, key, and materializer task.
    pub(crate) fn plan(&self) -> Result<PlannedBuild> {
        let arch = self.architecture.oci();
        let platform = format!("linux/{arch}");
        let total: usize = self
            .stages
            .iter()
            .map(|stage| 1 + stage.instructions.len())
            .sum();
        let mut index = 0usize;
        let mut all_nodes: BTreeMap<NodeKey, Node> = BTreeMap::new();
        let mut tasks: BTreeMap<NodeKey, StepTask> = BTreeMap::new();
        let mut stages = Vec::new();
        let mut built: Vec<BuiltStage> = Vec::new();
        let mut last = None;

        for stage in self.stages {
            index += 1;
            let place = StepPlace::new(index, total, stage.line);
            let from_args = match &stage.base {
                StageBase::Image(reference) => reference.clone(),
                StageBase::Stage(name) => name.clone(),
            };
            let start = StageStart {
                stage,
                arch,
                platform: &platform,
                place,
                built: &built,
            };
            let (config, current, base_digest, from_row) = self
                .begin_stage(start, &mut all_nodes, &mut tasks)
                .map_err(|error| crate::builder::at_from(error, place.line, &from_args))?;
            let mut folder = StageFolder::new(
                StageScope {
                    request: self.request,
                    context: self.context,
                    stages: self.stages,
                    built: &built,
                    arch,
                    architecture: self.architecture,
                    nodes: &mut all_nodes,
                    tasks: &mut tasks,
                },
                config,
                current,
            );
            for parsed in &stage.instructions {
                index += 1;
                let place = StepPlace::new(index, total, parsed.line);
                folder.fold(&parsed.instruction, place)?;
                last = Some(StepTrace {
                    phase: parsed.instruction.phase(),
                    command: parsed.instruction.command_text(),
                    line: parsed.line,
                });
            }

            let (config, current, mut rows) = folder.into_parts();
            if let Some(row) = from_row {
                rows.insert(0, row);
            }
            let image_canon = format!(
                "FROM {}",
                match &stage.base {
                    StageBase::Image(image) => image.clone(),
                    StageBase::Stage(name) => name.clone(),
                }
            );
            let image_key = NodeKey::image(arch, &current, &image_canon, &config.fingerprint());
            all_nodes.insert(
                image_key,
                Node {
                    key: image_key,
                    kind: NodeKind::Image,
                    parents: vec![current],
                    canon: image_canon,
                    line: line_u32(stage.line),
                },
            );
            tasks.insert(
                image_key,
                StepTask::Image {
                    config: config.clone(),
                },
            );
            let graph = BuildGraph::new(image_key, all_nodes.values().cloned().collect()).map_err(
                |error| Error::Interrupted {
                    message: error.to_string(),
                },
            )?;
            stages.push(PlannedStage {
                graph,
                head: image_key,
                rows,
            });
            built.push(BuiltStage {
                alias: stage.alias.clone(),
                key: current,
                config,
                base_digest,
            });
        }

        let final_stage = built.last().ok_or_else(|| Error::UnknownStage {
            name: "final".to_owned(),
        })?;
        Ok(PlannedBuild {
            stages,
            tasks,
            final_config: final_stage.config.clone(),
            final_key: final_stage.key,
            base_digest: final_stage.base_digest.clone(),
            last,
        })
    }

    /// Opens one stage: its base node (or inherited key) plus the `FROM` row.
    fn begin_stage(
        &self,
        start: StageStart<'_>,
        nodes: &mut BTreeMap<NodeKey, Node>,
        tasks: &mut BTreeMap<NodeKey, StepTask>,
    ) -> Result<(ImageConfig, NodeKey, Option<String>, Option<PlannedRow>)> {
        let StageStart {
            stage,
            arch,
            platform,
            place,
            built,
        } = start;
        match &stage.base {
            StageBase::Image(reference) if reference == "scratch" => {
                let key = NodeKey::base(arch, "scratch", platform);
                nodes.entry(key).or_insert_with(|| Node {
                    key,
                    kind: NodeKind::Base,
                    parents: Vec::new(),
                    canon: "FROM scratch".to_owned(),
                    line: line_u32(stage.line),
                });
                tasks.insert(
                    key,
                    StepTask::Base {
                        reference: reference.clone(),
                        digest: None,
                    },
                );
                Ok((
                    ImageConfig::default(),
                    key,
                    None,
                    Some(PlannedRow {
                        place,
                        keyword: "FROM",
                        args: reference.clone(),
                        key: Some(key),
                    }),
                ))
            }
            StageBase::Image(reference) => {
                let parsed = ImageReference::parse(reference)?;
                let resolved = self.resolver.resolve(&parsed)?;
                let key = NodeKey::base(arch, &resolved.digest, platform);
                nodes.entry(key).or_insert_with(|| Node {
                    key,
                    kind: NodeKind::Base,
                    parents: Vec::new(),
                    canon: format!("FROM {}", resolved.digest),
                    line: line_u32(stage.line),
                });
                tasks.insert(
                    key,
                    StepTask::Base {
                        reference: reference.clone(),
                        digest: Some(resolved.digest.clone()),
                    },
                );
                Ok((
                    ImageConfig::from(&resolved.facts),
                    key,
                    Some(resolved.digest),
                    Some(PlannedRow {
                        place,
                        keyword: "FROM",
                        args: reference.clone(),
                        key: Some(key),
                    }),
                ))
            }
            StageBase::Stage(name) => {
                let prior = built
                    .iter()
                    .find(|stage| {
                        stage
                            .alias
                            .as_deref()
                            .is_some_and(|alias| alias.eq_ignore_ascii_case(name))
                    })
                    .ok_or_else(|| Error::UnknownStage { name: name.clone() })?;
                Ok((
                    prior.config.clone(),
                    prior.key,
                    prior.base_digest.clone(),
                    Some(PlannedRow {
                        place,
                        keyword: "FROM",
                        args: name.clone(),
                        key: None,
                    }),
                ))
            }
        }
    }
}

/// Clamps a `Dockerfile` line to the `u32` node records store.
pub(crate) fn line_u32(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}
