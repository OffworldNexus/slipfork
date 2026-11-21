//! `fork` and `drop`: one CoW snapshot of project + world data, a branch switch, and one boot.
//!
//! The Btrfs ioctls stay in `slipfork_world` and branch topology stays behind the injected
//! `VcsWorkspace` port; this module only sequences them and maps every refusal to a typed `error`
//! frame. A fork's snapshot and its boot reservation happen under one `Core` guard.

use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_vcs::{Dirty, VcsError};
use slipfork_world::{
    BtrfsEngine, Dirtiness, Error as WorldError, ForkName, StoreLayout, WorldEngine, WorldName,
    is_subvolume_path,
};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::rpc::key_of;

/// One project snapshot that is ready to boot as its own world.
pub(crate) struct PreparedFork {
    pub(crate) parent: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) fork: String,
    pub(crate) fork_name: ForkName,
    pub(crate) world: String,
    pub(crate) world_name: WorldName,
    pub(crate) snapshot_ms: u64,
    pub(crate) started: Instant,
}

impl Core {
    /// Snapshots the fork and reserves its boot in the same locked step.
    ///
    /// The daemon boots the returned session through the ordinary `up` path, so a fork is never a
    /// second boot implementation. Any failure after the snapshot deletes both snapshots again.
    pub(crate) fn begin_fork(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<(BootSession, PreparedFork), Failure> {
        let prepared = self.snapshot(params, emit)?;
        let boot = json!({
            "project": prepared.project.clone(),
            "world": prepared.world.clone(),
        });
        match self.begin_up(&boot) {
            Ok(mut session) => {
                session.reserve_fork(prepared.fork.clone());
                Ok((session, prepared))
            }
            Err(failure) => {
                self.discard_fork(&prepared);
                Err(failure)
            }
        }
    }

    /// Deletes both snapshots of a fork that never published, so a retry is never blocked.
    pub(crate) fn discard_fork(&self, prepared: &PreparedFork) {
        let engine = BtrfsEngine::new(StoreLayout::at(self.host.store_root_for(&prepared.parent)));
        let _ = engine.drop_fork(
            &prepared.parent,
            &prepared.world_name,
            &prepared.fork_name,
            Dirtiness::Clean,
            true,
        );
    }

    /// Snapshots the project and its world data, then switches the fork checkout to its branch.
    fn snapshot(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<PreparedFork, Failure> {
        let project = project_param(params)?;
        // Probe before touching the mount store: a refusal must not create `.slipfork-store`.
        if !is_subvolume_path(&project).unwrap_or(false) {
            return Err(Failure::new(
                "not-btrfs",
                format!("`{}` is not a Btrfs subvolume", project.display()),
            ));
        }
        let parent = self
            .worlds
            .get(&key_of(&project))
            .map(|entity| entity.name().to_owned())
            .ok_or_else(|| Failure::new("no-world", "no world is running in this directory"))?;
        let world = WorldName::parse(&parent)
            .map_err(|error| Failure::new("invalid-world", first_line(&error.to_string())))?;
        let name = match param_name(params) {
            Some(name) => name.to_owned(),
            None => self
                .vcs
                .branch(&project)
                .map_err(|error| vcs_failure(&error))?,
        };
        let fork = ForkName::parse(&name)
            .map_err(|error| Failure::new("invalid-fork", error.to_string()))?;
        let engine = BtrfsEngine::new(StoreLayout::at(self.host.store_root_for(&project)));
        engine
            .ensure_store()
            .map_err(|error| btrfs_failure(&error, &project))?;
        if let Some(forks_dir) = StoreLayout::project_fork(&project, &fork).parent() {
            let _ = std::fs::create_dir_all(forks_dir);
        }
        // `specs/03` §1 step 1: flush dirty filesystem pages before snapshotting so on-disk pages
        // are consistent and `SNAP_CREATE_V2` commits only the snapshot metadata.
        if let Ok(dir) = File::open(&project) {
            // SAFETY: `dir` is a live directory descriptor; `syncfs` only flushes its filesystem.
            unsafe {
                libc::syncfs(dir.as_raw_fd());
            }
        }
        emit(json!({
            "event": "started", "step": 1, "total": 3, "kind": "fork",
            "label": format!("snapshotting {}", fork.as_str()),
        }));
        let started = Instant::now();
        let paths = engine
            .fork(&project, &world, &fork)
            .map_err(|error| btrfs_failure(&error, &project))?;
        let snapshot_ms = elapsed_ms(started);
        // Keep `.slipfork/*.stamp` hashes intact while stripping the parent world's live handle
        // and supervisor log so the fork starts with its own clean boot log.
        let _ = std::fs::remove_file(paths.project.join(".slipfork/world.json"));
        let _ = std::fs::remove_file(paths.project.join(".slipfork/supervisor.log"));
        emit(json!({
            "event": "finished", "step": 1, "total": 3, "kind": "fork",
            "label": "snapshot", "detail": "project + world data",
            "elapsed_ms": snapshot_ms,
        }));
        emit(json!({
            "event": "started", "step": 2, "total": 3, "kind": "git",
            "label": format!("switching the fork to {name}"),
        }));
        if let Err(failure) = self
            .vcs
            .switch(&paths.project, &name)
            .map_err(|error| vcs_failure(&error))
        {
            let _ = engine.drop_fork(&project, &world, &fork, Dirtiness::Clean, true);
            return Err(failure);
        }
        emit(json!({
            "event": "finished", "step": 2, "total": 3, "kind": "git",
            "label": format!("branch {name}"), "detail": "checked out",
            "elapsed_ms": elapsed_ms(started),
        }));
        Ok(PreparedFork {
            parent: project,
            project: paths.project,
            fork: fork.as_str().to_owned(),
            fork_name: fork.clone(),
            world: StoreLayout::fork_world_name(&world, &fork)
                .as_str()
                .to_owned(),
            world_name: world,
            snapshot_ms,
            started,
        })
    }

    /// Stops the fork when live, then removes both snapshots and its registry entry.
    pub(crate) fn drop_fork(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let project = project_param(params)?;
        if !is_subvolume_path(&project).unwrap_or(false) {
            return Err(Failure::new(
                "not-btrfs",
                format!("`{}` is not a Btrfs subvolume", project.display()),
            ));
        }
        let name =
            param_name(params).ok_or_else(|| Failure::new("bad-params", "`name` is required"))?;
        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fork = ForkName::parse(name)
            .map_err(|error| Failure::new("invalid-fork", error.to_string()))?;
        let fork_project = StoreLayout::project_fork(&project, &fork);
        let fork_key = key_of(&fork_project);
        let registered = self.worlds.contains_key(&fork_key);
        if !registered && !is_subvolume_path(&fork_project).unwrap_or(false) {
            return Err(Failure::new(
                "no-fork",
                format!("no fork named `{}` exists", fork.as_str()),
            ));
        }
        // Refuse dirty work before stopping anything: a refusal leaves the fork running.
        let dirtiness = if force {
            Dirtiness::Clean
        } else {
            match self.vcs.dirtiness(&fork_project) {
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
        let parent = self.parent_world(&project, fork.as_str())?;
        let world = WorldName::parse(&parent)
            .map_err(|error| Failure::new("invalid-world", first_line(&error.to_string())))?;
        emit(json!({
            "event": "started", "step": 1, "total": 2, "kind": "fork",
            "label": format!("stopping {}", fork.as_str()),
        }));
        if registered {
            self.stop_world(&fork_key, Duration::from_secs(60))
                .map_err(|failure| Failure::new("fork-busy", first_line(&failure.message)))?;
        }
        emit(json!({
            "event": "finished", "step": 1, "total": 2, "kind": "fork",
            "label": "stopped fork", "detail": "pods stopped",
            "elapsed_ms": elapsed_ms(started),
        }));
        emit(json!({
            "event": "started", "step": 2, "total": 2, "kind": "fork",
            "label": "deleting snapshots",
        }));
        let engine = BtrfsEngine::new(StoreLayout::at(self.host.store_root_for(&project)));
        engine
            .drop_fork(&project, &world, &fork, dirtiness, force)
            .map_err(|error| btrfs_failure(&error, &project))?;
        emit(json!({
            "event": "finished", "step": 2, "total": 2, "kind": "fork",
            "label": "deleted snapshots", "detail": "project + world data",
            "elapsed_ms": elapsed_ms(started),
        }));
        self.worlds.remove(&fork_key);
        self.ingress = None;
        self.persist()?;
        Ok(json!({
            "dropped": fork.as_str(),
            "elapsed_ms": elapsed_ms(started),
        }))
    }

    /// The world a fork belongs to: the live parent, else the fork's own registry record.
    fn parent_world(&self, project: &Path, fork: &str) -> Result<String, Failure> {
        if let Some(entity) = self.worlds.get(&key_of(project)) {
            return Ok(entity.name().to_owned());
        }
        for entity in self.worlds.values() {
            let World::Live { entry, .. } = entity else {
                continue;
            };
            if entry.fork.as_deref() != Some(fork) {
                continue;
            }
            if let Some((_, world)) = StoreLayout::parse_fork_world(OsStr::new(&entry.world)) {
                return Ok(world.as_str().to_owned());
            }
        }
        crate::world::world_name(project, None)
            .map(|name| name.as_str().to_owned())
            .map_err(|error| Failure::new("invalid-world", first_line(&error)))
    }
}

/// The `started`/`finished` phases of one fork measured from its own beginning.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn project_param(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

fn param_name(params: &Value) -> Option<&str> {
    params.get("name").and_then(Value::as_str)
}

/// Maps a Btrfs refusal to a typed code and a bare one-line message (the CLI owns the `next:`).
fn btrfs_failure(error: &WorldError, project: &Path) -> Failure {
    match error {
        WorldError::NotBtrfs { .. }
        | WorldError::CrossDevice { .. }
        | WorldError::NotASubvolume { .. } => Failure::new(
            "not-btrfs",
            format!("`{}` is not a Btrfs subvolume", project.display()),
        ),
        WorldError::RefusedDirtyCheckout { target } => {
            Failure::new("dirty-fork", format!("{target} has uncommitted changes"))
        }
        other => Failure::new("fork-failed", first_line(&other.to_string())),
    }
}

/// Maps a `VcsWorkspace` refusal to the daemon's typed frame codes.
fn vcs_failure(error: &VcsError) -> Failure {
    match error {
        VcsError::Unavailable { .. } => Failure::new("git-missing", "`git` is not on PATH"),
        VcsError::NoBranch { .. } => Failure::new("git-branch", first_line(&error.to_string())),
        VcsError::Switch { .. } | VcsError::Command { .. } => {
            Failure::new("git-switch", first_line(&error.to_string()))
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("fork failed").to_owned()
}
