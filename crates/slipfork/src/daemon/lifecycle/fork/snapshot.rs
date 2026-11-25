//! The project + world CoW snapshot and branch switch that leave a fork ready to boot.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;
use slipfork_world::{
    BtrfsEngine, Dirtiness, ForkName, PostgresLauncher, StoreLayout, WorldEngine, WorldName,
};

use super::super::core::Core;
use super::super::plan;
use super::failure::{btrfs_failure, first_line, vcs_failure};
use super::target::ForkTarget;
use crate::daemon::Failure;
use crate::world::elapsed_ms;

/// One project snapshot that is ready to boot as its own world.
pub(in crate::daemon) struct PreparedFork {
    /// The family root checkout, which owns both `.forks/<name>` and the store.
    pub(in crate::daemon) parent: PathBuf,
    pub(in crate::daemon) project: PathBuf,
    pub(in crate::daemon) fork: String,
    pub(in crate::daemon) fork_name: ForkName,
    /// The fork's world id (`<fork>--<root>`), taken from [`slipfork_world::ForkPaths`].
    pub(in crate::daemon) world: String,
    /// The family root world every fork world id is suffixed with.
    pub(in crate::daemon) root_world: WorldName,
    /// The immediate parent world (`origin` of the fork's lease).
    pub(in crate::daemon) parent_world: WorldName,
    pub(in crate::daemon) snapshot_ms: u64,
    /// Pre-fork preparation time (branch/slug resolution). No SQL runs here.
    pub(in crate::daemon) preflight_ms: u64,
    pub(in crate::daemon) started: Instant,
}

impl Core {
    /// Snapshots the project and its world data, then switches the fork checkout to its branch.
    pub(super) fn snapshot(
        &self,
        target: ForkTarget,
        name: &str,
        fork: ForkName,
        preflight_ms: u64,
        rows: &[plan::PlannedRow],
        emit: &mut dyn FnMut(Value),
    ) -> Result<PreparedFork, Failure> {
        let ForkTarget {
            project,
            world,
            store_root,
            pods_dir,
            vcs,
            started,
            ..
        } = target;
        let engine = BtrfsEngine::new(StoreLayout::at(store_root.clone()));
        engine
            .ensure_store()
            .map_err(|error| btrfs_failure(&error, &project))?;
        if let Some(forks_dir) = StoreLayout::project_fork(&project, &fork).parent() {
            let _ = std::fs::create_dir_all(forks_dir);
        }
        // PostgreSQL writes a fresh commit from `shared_buffers`/`wal_buffers` only on checkpoint,
        // so this `< 5ms` direct-TCP `CHECKPOINT` must land before any `SNAP_CREATE_V2`.
        PostgresLauncher::new(StoreLayout::at(store_root), world.clone())
            .checkpoint_live(&pods_dir)
            .map_err(|error| {
                Failure::new(
                    "pg-checkpoint",
                    format!(
                        "could not flush postgres buffers before the fork: {}",
                        first_line(&error.to_string())
                    ),
                )
            })?;
        let snapshot = plan::PrepFrame::planned(rows, "fork", "snapshot", 1);
        emit(snapshot.started(format!("snapshotting {}", fork.as_str())));
        let snapshot_started = Instant::now();
        let paths = engine
            .fork(&project, &world, &fork)
            .map_err(|error| btrfs_failure(&error, &project))?;
        let snapshot_ms = elapsed_ms(snapshot_started);
        // Keep `.slipfork/*.stamp` hashes intact while stripping the parent world's live
        // supervisor log so the fork starts with its own clean boot log. The world handle lives in
        // the ledger row, never in a checkout file.
        let _ = std::fs::remove_file(paths.project.join(".slipfork/supervisor.log"));
        emit(snapshot.finished("snapshot".to_owned(), "project + world data", snapshot_ms));
        let git_started = Instant::now();
        let git_name = format!("branch {name}");
        let git = plan::PrepFrame::planned(rows, "git", &git_name, 2);
        emit(git.started(format!("switching the fork to {name}")));
        if let Err(failure) = vcs
            .switch(&paths.project, name)
            .map_err(|error| vcs_failure(&error))
        {
            let _ = engine.drop_fork(&project, &world, &fork, Dirtiness::Clean, true);
            return Err(failure);
        }
        emit(git.finished(git_name.clone(), "checked out", elapsed_ms(git_started)));
        // `ForkPaths` is authoritative: the family root owns the store, the parent names the
        // lease origin, and the world snapshot's file name is the fork's world id.
        let world = paths
            .world
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| WorldName::parse(name).ok())
            .map_or_else(
                || {
                    StoreLayout::fork_world_name(&paths.root, &fork)
                        .as_str()
                        .to_owned()
                },
                |world| world.as_str().to_owned(),
            );
        Ok(PreparedFork {
            parent: slipfork_world::fork_checkout(&project)
                .map_or_else(|| project.clone(), |(root, _)| root),
            project: paths.project,
            fork: fork.as_str().to_owned(),
            fork_name: fork.clone(),
            world,
            root_world: paths.root,
            parent_world: paths.parent,
            snapshot_ms,
            preflight_ms,
            started,
        })
    }
}
