//! Applying one [`StepTask`] into a writable snapshot slot.

use std::cell::RefCell;
use std::fs;
use std::path::Path;

use slipfork_world::{Dirtiness, Facts, Node, NodeKey, SnapshotStore, WorldEngine};

use crate::cache_mounts::CacheMounts;
use crate::context::BuildContext;
use crate::copy::{self, Destination};
use crate::dockerfile::{CopyOptions, ImageConfig, Instruction, Owner};
use crate::error::{Error, Result};
use crate::identity::Identity;
use crate::plan::{CopyOrigin, FsKind, StepTask};
use crate::puller::OciPuller;
use crate::reference::ImageReference;
use crate::runtime::CrunStepRuntime;
use crate::step_log::StepLog;

use super::diagnostics::at_line;

/// The injected dependencies one materialization needs.
pub(crate) struct TaskMaterializer<'a, 's> {
    pub(crate) world: &'a dyn WorldEngine,
    pub(crate) puller: &'a OciPuller,
    pub(crate) store: &'a SnapshotStore<'s>,
    pub(crate) context: &'a BuildContext,
    pub(crate) owner: Owner,
    pub(crate) runtime: &'a CrunStepRuntime,
    pub(crate) cache_mounts: &'a CacheMounts,
}

/// The expanded row one filesystem task occupies.
struct FsRow<'a> {
    instruction: &'a Instruction,
    config: &'a ImageConfig,
    kind: &'a FsKind,
    line: usize,
}

impl TaskMaterializer<'_, '_> {
    /// Applies `task` inside `slot`, returning the facts the node record stores.
    pub(crate) fn apply(
        &self,
        node: &Node,
        task: &StepTask,
        slot: &Path,
        log: &RefCell<&mut StepLog<'_>>,
    ) -> Result<Facts> {
        match task {
            StepTask::Base {
                reference, digest, ..
            } => {
                if reference == "scratch" {
                    return Ok(serde_json::Value::Null);
                }
                let parsed = ImageReference::parse(reference)?;
                let Some(expected) = digest.as_deref() else {
                    return Err(Error::Interrupted {
                        message: format!(
                            "`FROM {reference}` has no planned digest\nnext: report this with `slipfork doctor`"
                        ),
                    });
                };
                let pulled = self.puller.unpack_digest_into(
                    &parsed,
                    expected,
                    slot,
                    &mut log.borrow_mut(),
                )?;
                if pulled.digest != expected {
                    return Err(Error::MovedTag {
                        reference: reference.clone(),
                        expected: expected.to_owned(),
                        actual: pulled.digest,
                    });
                }
                Ok(serde_json::Value::Null)
            }
            StepTask::External { rootfs } => {
                self.world.destroy(slot, Dirtiness::Clean, true)?;
                self.world.clone_subvolume(rootfs, slot)?;
                Ok(serde_json::Value::Null)
            }
            StepTask::Image { config } => {
                serde_json::to_value(config).map_err(|source| Error::Json {
                    url: "image config".to_owned(),
                    source,
                })
            }
            StepTask::Fs {
                instruction,
                config,
                place,
                kind,
            } => {
                let row = FsRow {
                    instruction,
                    config,
                    kind,
                    line: place.line,
                };
                self.apply_fs(node, slot, row, log)
                    .map_err(|error| at_line(error, place.line, instruction))?;
                Ok(serde_json::Value::Null)
            }
        }
    }

    fn apply_fs(
        &self,
        node: &Node,
        slot: &Path,
        row: FsRow<'_>,
        log: &RefCell<&mut StepLog<'_>>,
    ) -> Result<()> {
        let FsRow {
            instruction,
            config,
            kind,
            line,
        } = row;
        match (instruction, kind) {
            (Instruction::Workdir(_), FsKind::Workdir) => {
                let target = slot.join(config.workdir.trim_start_matches('/'));
                fs::create_dir_all(&target).map_err(|source| Error::io("create", &target, source))
            }
            (Instruction::Run(run), FsKind::Run) => {
                let mut mounts = Vec::with_capacity(run.mounts.len());
                for mount in &run.mounts {
                    let id = mount.id.clone().unwrap_or_else(|| mount.target.clone());
                    mounts.push((mount.clone(), self.cache_mounts.prepare(&id)?));
                }
                let id = run_id(&node.key);
                let mut guard = log.borrow_mut();
                self.runtime
                    .run(&id, slot, config, run, &mounts, line, &mut guard)
            }
            (Instruction::Copy(spec), FsKind::Copy(origin)) => {
                self.copy(node, slot, spec, config, origin.as_ref(), false)
            }
            (Instruction::Add(spec), FsKind::Add(origin)) => {
                self.copy(node, slot, spec, config, origin.as_ref(), true)
            }
            _ => Err(Error::Interrupted {
                message: format!(
                    "the plan paired `{}` with the wrong materializer\nnext: report this with `slipfork doctor`",
                    instruction.keyword()
                ),
            }),
        }
    }

    fn copy(
        &self,
        node: &Node,
        slot: &Path,
        spec: &crate::dockerfile::CopySpec,
        config: &ImageConfig,
        origin: Option<&CopyOrigin>,
        is_add: bool,
    ) -> Result<()> {
        if let Some(account) = spec.chown.as_deref() {
            let parent = node
                .parents
                .first()
                .map_or_else(|| slot.to_path_buf(), |key| self.store.node_dir(key));
            Identity::resolve(&parent, Some(account))?;
        }
        let options = CopyOptions::new(self.owner, spec.mode);
        let destination = Destination::new(slot, &config.workdir);
        match origin {
            Some(CopyOrigin::Snapshot(key)) => copy::copy_rootfs(
                &self.store.node_dir(key),
                &spec.sources,
                &spec.dest,
                destination,
                options,
            ),
            Some(CopyOrigin::External { rootfs, .. }) => {
                copy::copy_rootfs(rootfs, &spec.sources, &spec.dest, destination, options)
            }
            None if is_add => copy::add_context(
                self.context,
                &spec.sources,
                &spec.dest,
                destination,
                options,
            ),
            None => copy::copy_context(
                self.context,
                &spec.sources,
                &spec.dest,
                destination,
                options,
            ),
        }
    }
}

/// Stable `crun` container id for one step key.
fn run_id(key: &NodeKey) -> String {
    format!("sf-{}-{}", std::process::id(), &key.as_hex()[..16])
}
