//! The validated fork target, resolved under a brief `Core` lock before `CHECKPOINT` runs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use slipfork_vcs::VcsWorkspace;
use slipfork_world::{ForkName, StoreLayout, WorldName};

use super::super::core::{Core, World};
use super::failure::{first_line, vcs_failure};
use super::{param_name, project_param};
use crate::daemon::Failure;
use crate::daemon::rpc::key_of;
use crate::world::elapsed_ms;
use crate::world::features::WorldFeatures;

/// Validated fork target resolved under a brief `Core` lock so `git` branch discovery runs with
/// zero daemon locks held.
pub(in crate::daemon) struct ForkTarget {
    pub(super) project: PathBuf,
    pub(super) world: WorldName,
    pub(super) store_root: PathBuf,
    pub(super) pods_dir: PathBuf,
    pub(super) requested_name: Option<String>,
    pub(super) vcs: Arc<dyn VcsWorkspace>,
    pub(super) started: Instant,
    /// Feature override typed on the `fork` request; `None` inherits the snapshot's file.
    pub(super) features: Option<WorldFeatures>,
}

impl ForkTarget {
    /// Resolves the fork slug outside all locks, returning `(branch_name, fork, preflight_ms)`.
    pub(in crate::daemon) fn prepare_outside_locks(
        &self,
    ) -> Result<(String, ForkName, u64), Failure> {
        let started = Instant::now();
        let raw_name = match &self.requested_name {
            Some(name) => name.clone(),
            None => {
                let branch = self
                    .vcs
                    .branch(&self.project)
                    .map_err(|error| vcs_failure(&error))?;
                slipfork_forkfile::DnsLabel::slug(&branch)
                    .map_err(|error| Failure::new("invalid-fork", error.message(&branch)))?
            }
        };
        let fork = ForkName::parse(&raw_name)
            .map_err(|error| Failure::new("invalid-fork", error.to_string()))?;
        Ok((raw_name, fork, elapsed_ms(started)))
    }
}

impl Core {
    /// Validates the live parent world under a short lock before `CHECKPOINT` runs unlocked.
    pub(in crate::daemon) fn inspect_fork(&self, params: &Value) -> Result<ForkTarget, Failure> {
        let started = Instant::now();
        let project = project_param(params)?;
        let (parent, pods_dir) = match self.worlds.get(&key_of(&project)) {
            Some(World::Live { row, .. }) => {
                let Some(pods_dir) = row.pods_dir.clone() else {
                    return Err(Failure::new(
                        "no-world",
                        "no world is running in this directory",
                    ));
                };
                (row.world_name.clone(), pods_dir)
            }
            Some(World::Booting { row, .. }) => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{}` is still starting", row.world_name),
                ));
            }
            None => {
                return Err(Failure::new(
                    "no-world",
                    "no world is running in this directory",
                ));
            }
        };
        let world = WorldName::parse(&parent)
            .map_err(|error| Failure::new("invalid-world", first_line(&error.to_string())))?;
        let features =
            WorldFeatures::from_params(params).map_err(|error| Failure::new("feature", error))?;
        Ok(ForkTarget {
            store_root: self.host.store_root_for(&project),
            project,
            world,
            pods_dir,
            requested_name: param_name(params).map(str::to_owned),
            vcs: Arc::clone(&self.vcs),
            started,
            features,
        })
    }

    /// Whether any artifact of `fork` already exists: a registry entry, the `.forks/<name>`
    /// checkout, the fork world snapshot, or a leftover resource clone from an earlier fork.
    ///
    /// `fork` snapshots into those exact paths, so a duplicate name must be refused before any
    /// work starts; the remedy is the one verb that removes all four (`slipfork drop <name> -f`,
    /// because a live fork's checkout carries its own run state and is never VCS-clean).
    pub(super) fn fork_artifacts_exist(&self, target: &ForkTarget, fork: &ForkName) -> bool {
        let checkout = StoreLayout::project_fork(&target.project, fork);
        if self.worlds.contains_key(&key_of(&checkout)) || checkout.exists() {
            return true;
        }
        let layout = StoreLayout::at(target.store_root.clone());
        if layout.fork_world(&target.world, fork).exists() {
            return true;
        }
        let fork_world = StoreLayout::fork_world_name(&target.world, fork);
        std::fs::read_dir(layout.worlds_root())
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                StoreLayout::parse_world_clone(&entry.file_name())
                    .is_some_and(|(world, _, _)| world == fork_world)
            })
    }
}
