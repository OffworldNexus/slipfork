//! `drop`: stop a live fork, then delete both of its CoW snapshots and its single ledger row.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_vcs::Dirty;
use slipfork_world::{
    BtrfsEngine, Dirtiness, ForkName, LedgerError, Resolved, RuntimePods, StoreLayout, WorldEngine,
    WorldName, is_subvolume_path,
};

use super::super::core::Core;
use super::super::plan;
use super::failure::{btrfs_failure, first_line};
use super::snapshot::PreparedFork;
use super::{param_name, project_param};
use crate::daemon::Failure;
use crate::daemon::rpc::key_of;
use crate::world::elapsed_ms;

impl Core {
    /// Deletes every trace of a fork that never published, so a retry is never blocked.
    ///
    /// The order is the invariant: `stop_world` shuts the supervisor's control plane down first
    /// (a `Booting` supervisor is waited on and its `-pgid` group killed, a `Live` one also drains
    /// its pods), then the registry entry is dropped and persisted, and only then is the tmpfs pod
    /// root purged and the Btrfs snapshots unlinked. Unlinking a still-running world's store
    /// would orphan a supervisor whose sockets and pod bundles live under the deleted tree.
    pub(in crate::daemon) fn discard_fork(&mut self, prepared: &PreparedFork) {
        let key = key_of(&prepared.project);
        // A boot that never registered answers `no-world`; the purge below still has to run.
        let _ = self.stop_world(&key, Duration::from_secs(10), &mut |_, _| {});
        self.remove_lease(&prepared.project, &prepared.world);
        self.worlds.remove(&key);
        self.ingress = None;
        // The ledger row exists only once `begin_fork` reserved it; an earlier failure has none.
        if let Some(row) = self.row_for_project(&prepared.project) {
            let outcome = if self.is_fork_row(row.id) {
                self.ledger.delete_world_row(row.id)
            } else {
                self.ledger.mark_stopped(row.id, crate::daemon::now_ms())
            };
            if let Err(error) = outcome {
                eprintln!(
                    "slipfork daemon: {}",
                    error.to_string().lines().next().unwrap_or_default()
                );
            }
        }
        let forked = StoreLayout::fork_world_name(&prepared.root_world, &prepared.fork_name);
        Self::report_retain(self.retain_runs(&prepared.parent, forked.as_str()));
        let _ = RuntimePods::purge(&self.host.runtime_dir, &forked);
        let engine = BtrfsEngine::new(StoreLayout::at(self.host.store_root_for(&prepared.parent)));
        let _ = engine.drop_fork(
            &prepared.parent,
            &prepared.root_world,
            &prepared.fork_name,
            Dirtiness::Clean,
            true,
        );
    }

    /// Stops the fork when live, then removes every fork artifact and its single ledger row.
    pub(in crate::daemon) fn drop_fork(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let project = project_param(params)?;
        let name =
            param_name(params).ok_or_else(|| Failure::new("bad-params", "`name` is required"))?;
        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fork = ForkName::parse(name)
            .map_err(|error| Failure::new("invalid-fork", error.to_string()))?;
        // The family root owns both `<root>/.forks/<name>` and the world store.
        let root = slipfork_world::fork_checkout(&project)
            .map_or_else(|| project.clone(), |(root, _)| root);
        let rows = self
            .ledger
            .list()
            .map_err(crate::daemon::lifecycle::core::ledger_failure)?;
        // One resolver owns every target rule: the family root refuses, a fork row names its own
        // checkout, and a vanished row falls through to the artifact-only ghost path.
        let scope = slipfork_world::ProjectScope::resolve(&project);
        let resolved = match self.ledger.resolve(&scope, name) {
            Ok(resolved) => resolved,
            Err(LedgerError::UnknownTarget { .. }) => None,
            Err(error) => return Err(crate::daemon::lifecycle::core::ledger_failure(error)),
        };
        let (checkout, fork_row, root_checkout, root_world) = match resolved {
            // A row without a `forks` edge is a root: `drop` never deletes a root world.
            Some(Resolved { fork: None, .. }) => {
                return Err(Failure::new(
                    "root-world",
                    format!(
                        "{name} is a root world; drop only removes forks\nnext: slipfork down {name}"
                    ),
                ));
            }
            Some(Resolved {
                world,
                fork: Some(fork),
            }) => {
                let row = rows.worlds.iter().find(|row| row.id == fork.root_id).ok_or_else(|| {
                    Failure::new(
                        "ledger",
                        "the fork's root row is missing\nnext: run `slipfork up` in the root checkout",
                    )
                })?;
                let world_root = WorldName::parse(&row.world_name).map_err(|error| {
                    Failure::new("invalid-world", first_line(&error.to_string()))
                })?;
                (
                    world.checkout_path.clone(),
                    Some(fork),
                    row.checkout_path.clone(),
                    world_root,
                )
            }
            // Artifact-only ghost: no row survives, so only the canonical paths can be swept.
            None => {
                let root_world_name = rows
                    .worlds
                    .iter()
                    .find(|row| row.checkout_path == root)
                    .map(|row| row.world_name.clone())
                    .or_else(|| {
                        root.file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                let world_root = WorldName::parse(&root_world_name).map_err(|error| {
                    Failure::new("invalid-world", first_line(&error.to_string()))
                })?;
                (
                    StoreLayout::project_fork(&root, &fork),
                    None,
                    root.clone(),
                    world_root,
                )
            }
        };
        let layout = StoreLayout::at(self.host.store_root_for(&root_checkout));
        let fork_world = layout.fork_world(&root_world, &fork);
        let has_checkout = checkout.is_dir();
        let has_world = is_subvolume_path(&fork_world).unwrap_or(false);
        if fork_row.is_none() && !has_checkout && !has_world {
            return Err(Failure::new(
                "no-fork",
                format!("no fork named `{}` exists", fork.as_str()),
            ));
        }
        // Refuse dirty work before stopping anything: a refusal leaves the fork running.
        let dirtiness = if force || !has_checkout {
            Dirtiness::Clean
        } else {
            match self.vcs.dirtiness(&checkout) {
                Ok(Dirty::Clean) => Dirtiness::Clean,
                // A failed probe is dirty work: never discard what cannot be proven clean.
                Ok(Dirty::Dirty) | Err(_) => Dirtiness::Dirty,
            }
        };
        if dirtiness.is_dirty() {
            return Err(Failure::new(
                "dirty-fork",
                format!("fork `{}` has uncommitted changes", fork.as_str()),
            ));
        }
        // One two-row plan before the first `started`: `stop` owns row 1, `delete` row 2, so every
        // frame echoes `[i/2]` and the fork name is the target on both phases.
        let plan = plan::drop_plan(fork.as_str());
        plan::emit(&plan.rows, plan.rows.len(), emit);
        emit(plan.stop.started(format!("stopping {}", fork.as_str())));
        let stop_started = Instant::now();
        let fork_key = key_of(&checkout);
        if self.worlds.contains_key(&fork_key) {
            self.stop_world(&fork_key, Duration::from_secs(60), &mut |_, _| {})
                .map_err(|failure| Failure::new("fork-busy", first_line(&failure.message)))?;
        }
        emit(plan.stop.finished(
            "stopped".to_owned(),
            "pods stopped",
            elapsed_ms(stop_started),
        ));
        emit(plan.delete.started(format!("deleting {}", fork.as_str())));
        let delete_started = Instant::now();
        let forked = StoreLayout::fork_world_name(&root_world, &fork);
        Self::report_retain(self.retain_runs(&root_checkout, forked.as_str()));
        let engine = BtrfsEngine::new(layout);
        engine
            .drop_fork(&root_checkout, &root_world, &fork, dirtiness, force)
            .map_err(|error| btrfs_failure(&error, &root_checkout))?;
        emit(plan.delete.finished(
            "deleted".to_owned(),
            "project + world data",
            elapsed_ms(delete_started),
        ));
        // `delete_world_row` splices the fork's children in one transaction, so no hand-reparent.
        if let Some(fork) = &fork_row {
            self.ledger
                .delete_world_row(fork.world_id)
                .map_err(crate::daemon::lifecycle::core::ledger_failure)?;
        }
        self.worlds.remove(&fork_key);
        self.ingress = None;
        Ok(json!({
            "dropped": fork.as_str(),
            "elapsed_ms": elapsed_ms(started),
        }))
    }
}
