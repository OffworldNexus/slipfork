//! `fork` and `drop`: one CoW snapshot of project + world data, a branch switch, and one boot.
//!
//! PostgreSQL keeps a just-committed row in user-space `shared_buffers`/`wal_buffers` until a
//! checkpoint writes it into the kernel page cache, so `syncfs(2)` alone cannot see it. Before
//! `BtrfsEngine::fork` snapshots anything, the daemon dials `127.0.0.1:5432` through
//! `<pods>/control.sock` (`SCM_RIGHTS`) and speaks the PostgreSQL v3 wire protocol directly: one
//! `CHECKPOINT` in `< 5ms` with zero `crun`/`psql` spawn, then the fork's `syncfs` commits that
//! page cache. Btrfs ioctls stay inside `slipfork_world` and branch topology stays behind the
//! injected `VcsWorkspace` port.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_vcs::{Dirty, VcsError, VcsWorkspace};
use slipfork_world::{
    BtrfsEngine, Dirtiness, Error as WorldError, ForkName, LedgerError, PostgresLauncher, Resolved,
    RuntimePods, StoreLayout, WorldEngine, WorldName, is_subvolume_path,
};

use super::core::{Core, World};
use super::plan;
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::rpc::key_of;
use crate::world::features::WorldFeatures;

/// Announced plan row explaining why a feature-overridden fork rebuilds instead of adopting
/// the parent's sealed images in 0ms.
const FEATURE_REBUILD_ROW: (&str, &str) = ("reuse", "rebuilding images (feature set changed)");

/// Validated fork target resolved under a brief `Core` lock so `git` branch discovery runs with
/// zero daemon locks held.
pub(crate) struct ForkTarget {
    pub(crate) project: PathBuf,
    pub(crate) world: WorldName,
    pub(crate) store_root: PathBuf,
    pub(crate) pods_dir: PathBuf,
    pub(crate) requested_name: Option<String>,
    pub(crate) vcs: Arc<dyn VcsWorkspace>,
    pub(crate) started: Instant,
    /// Feature override typed on the `fork` request; `None` inherits the snapshot's file.
    pub(crate) features: Option<WorldFeatures>,
}

/// One project snapshot that is ready to boot as its own world.
pub(crate) struct PreparedFork {
    /// The family root checkout, which owns both `.forks/<name>` and the store.
    pub(crate) parent: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) fork: String,
    pub(crate) fork_name: ForkName,
    /// The fork's world id (`<fork>--<root>`), taken from [`slipfork_world::ForkPaths`].
    pub(crate) world: String,
    /// The family root world every fork world id is suffixed with.
    pub(crate) root_world: WorldName,
    /// The immediate parent world (`origin` of the fork's lease).
    pub(crate) parent_world: WorldName,
    pub(crate) snapshot_ms: u64,
    /// Pre-fork preparation time (branch/slug resolution). No SQL runs here.
    pub(crate) preflight_ms: u64,
    pub(crate) started: Instant,
}

impl ForkTarget {
    /// Resolves the fork slug outside all locks, returning `(branch_name, fork, preflight_ms)`.
    pub(crate) fn prepare_outside_locks(&self) -> Result<(String, ForkName, u64), Failure> {
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
    pub(crate) fn inspect_fork(&self, params: &Value) -> Result<ForkTarget, Failure> {
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

    /// Snapshots the fork and reserves its boot in one locked step after `CHECKPOINT` finished.
    ///
    /// The daemon boots the returned session through the ordinary `up` path, so a fork is never a
    /// second boot implementation. Any failure after the snapshot deletes every fork subvolume.
    pub(crate) fn begin_fork(
        &mut self,
        target: ForkTarget,
        raw_name: &str,
        fork: ForkName,
        preflight_ms: u64,
        emit: &mut dyn FnMut(Value),
    ) -> Result<(BootSession, PreparedFork), Failure> {
        if !self.worlds.contains_key(&key_of(&target.project)) {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        }
        // Refuse a duplicate name before the first progress row: an existing checkout, world
        // snapshot, or leftover clone would otherwise fail the snapshot mid-stream with a raw
        // `File exists (os error 17)`.
        if self.fork_artifacts_exist(&target, &fork) {
            return Err(Failure::new(
                "fork-exists",
                format!(
                    "fork \"{}\" already exists\nnext: slipfork drop {} -f",
                    fork.as_str(),
                    fork.as_str()
                ),
            ));
        }
        if let Some(request) = &target.features {
            Self::validate_features(&target.project, request)?;
        }
        // One plan frame, before the snapshot mutates anything: the fork's own prep rows, an
        // explicit reuse-declined notice when feature overrides force image rebuilds, plus every
        // world step the fork will boot under its resolved feature set.
        let rows = Self::planned_fork_rows(&target.project, raw_name, target.features.as_ref());
        plan::emit(&rows, rows.len(), emit);
        let request = target.features.clone();
        let prepared = self.snapshot(target, raw_name, fork, preflight_ms, &rows, emit)?;
        if let Some(request) = request {
            if let Err(failure) = Self::apply_features(&prepared.project, request) {
                self.discard_fork(&prepared);
                return Err(failure);
            }
            emit(json!({
                "event": "up-to-date",
                "kind": FEATURE_REBUILD_ROW.0,
                "name": FEATURE_REBUILD_ROW.1,
                "state": "declined",
                "elapsed_ms": 0,
                "duration_ms": 0,
            }));
        }
        // The fork checkout exists only after `snapshot`; seal its heads (parent image nodes are
        // warm, so this is `cached`) before spawning `__supervise`. A failure rolls the fork back
        // instead of booting from a half-built head set.
        let fork_world = WorldName::parse(&prepared.world)
            .map_err(|error| Failure::new("invalid-world", first_line(&error.to_string())))?;
        let sealed = crate::daemon::reconcile::WorldBuild {
            host: &self.host,
            project: &prepared.project,
            world: &fork_world,
            rows: &rows,
        }
        .seal(false, emit);
        if let Err(failure) = sealed {
            self.discard_fork(&prepared);
            return Err(failure);
        }
        let planned = match crate::daemon::lease::prepare(
            &self.host,
            &prepared.project,
            &fork_world,
            Some(&prepared.parent_world),
        ) {
            Ok(planned) => planned,
            Err(failure) => {
                self.discard_fork(&prepared);
                return Err(failure);
            }
        };
        if let Err(error) =
            crate::daemon::lease::collect_garbage(&self.host, &prepared.project, Some(&planned.1))
        {
            eprintln!("slipfork daemon: snapshot gc: {error}");
        }
        let boot = json!({
            "project": prepared.project.clone(),
            "world": prepared.world.clone(),
        });
        let Some(root_row) = self.root_row(&prepared.parent) else {
            self.discard_fork(&prepared);
            return Err(Failure::new(
                "no-world",
                "the fork's root world has no ledger row\nnext: run `slipfork up` in the root checkout, then retry `slipfork fork`",
            ));
        };
        // The immediate parent edge comes from the snapshot's own `ForkPaths.parent`; a fork of a
        // fork keeps its real ancestry instead of flattening every child onto the root.
        let parent_id = self
            .ledger
            .list()
            .ok()
            .and_then(|rows| {
                rows.worlds
                    .into_iter()
                    .find(|row| row.world_name == prepared.parent_world.as_str())
                    .map(|row| row.id)
            })
            .unwrap_or(root_row.id);
        let fork_row = match self.ledger.begin_fork(slipfork_world::ForkRequest {
            root_id: root_row.id,
            parent_id,
            name: prepared.fork_name.clone(),
            runtime_dir: self.host.runtime_dir.clone(),
            branch: raw_name.to_owned(),
            now_ms: crate::daemon::now_ms(),
        }) {
            Ok(row) => row,
            Err(error) => {
                self.discard_fork(&prepared);
                return Err(crate::daemon::lifecycle::core::ledger_failure(error));
            }
        };
        match self.begin_up(&boot, planned, Some(fork_row.world_id)) {
            Ok(mut session) => {
                session.reserve_fork(prepared.fork.clone());
                session.plan_already_announced();
                Ok((session, prepared))
            }
            Err(failure) => {
                self.discard_fork(&prepared);
                Err(failure)
            }
        }
    }

    /// Whether any artifact of `fork` already exists: a registry entry, the `.forks/<name>`
    /// checkout, the fork world snapshot, or a leftover resource clone from an earlier fork.
    ///
    /// `fork` snapshots into those exact paths, so a duplicate name must be refused before any
    /// work starts; the remedy is the one verb that removes all four (`slipfork drop <name> -f`,
    /// because a live fork's checkout carries its own run state and is never VCS-clean).
    fn fork_artifacts_exist(&self, target: &ForkTarget, fork: &ForkName) -> bool {
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

    /// Builds the complete announced row list for one fork, inserting the reuse-declined row
    /// whenever the request carries feature overrides.
    fn planned_fork_rows(
        project: &Path,
        raw_name: &str,
        request: Option<&WorldFeatures>,
    ) -> Vec<plan::PlannedRow> {
        let mut rows = plan::fork_rows(raw_name);
        if request.is_some() {
            rows.push((
                FEATURE_REBUILD_ROW.0.to_owned(),
                FEATURE_REBUILD_ROW.1.to_owned(),
            ));
        }
        rows.extend(plan::world_rows_with(project, request));
        rows
    }

    /// Validates `request` merged over `<project>/.slipfork/features.json` without mutating disk.
    fn validate_features(
        project: &Path,
        request: &WorldFeatures,
    ) -> Result<WorldFeatures, Failure> {
        let config = WorldFeatures::merged(project, request).map_err(|error| {
            Failure::new(
                "feature",
                format!("{error}\nnext: {}", WorldFeatures::REMEDY),
            )
        })?;
        let path = project.join("Forkfile");
        let source = std::fs::read_to_string(&path).map_err(|_| {
            Failure::new(
                "forkfile",
                "cannot read `Forkfile`\nnext: restore the file, then retry `slipfork fork`",
            )
        })?;
        let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
            Failure::new(
                "feature",
                diagnostics
                    .first()
                    .map_or("invalid Forkfile".to_owned(), |item| {
                        format!("Forkfile:{}: {}", item.span.line(), item.message)
                    }),
            )
        })?;
        config.resolve(&checked.document).map_err(|diagnostic| {
            Failure::new(
                "feature",
                format!(
                    "Forkfile:{}: {}\nnext: {}",
                    diagnostic.span.line(),
                    diagnostic.message,
                    diagnostic.next
                ),
            )
        })?;
        Ok(config)
    }

    /// Applies a fork request's edits over the snapshot's inherited configuration, verifies it
    /// resolves against the fork checkout's `Forkfile`, and persists `.slipfork/features.json`.
    fn apply_features(project: &Path, request: WorldFeatures) -> Result<WorldFeatures, Failure> {
        let config = Self::validate_features(project, &request)?;
        config.save(project).map_err(|error| {
            Failure::new(
                "feature",
                format!(
                    "cannot save `{}`: {error}\nnext: check that the fork's `.slipfork` directory is writable",
                    WorldFeatures::path(project).display()
                ),
            )
        })?;
        Ok(config)
    }

    /// Deletes every trace of a fork that never published, so a retry is never blocked.
    ///
    /// The order is the invariant: `stop_world` shuts the supervisor's control plane down first
    /// (a `Booting` supervisor is waited on and its `-pgid` group killed, a `Live` one also drains
    /// its pods), then the registry entry is dropped and persisted, and only then is the tmpfs pod
    /// root purged and the Btrfs snapshots unlinked. Unlinking a still-running world's store
    /// would orphan a supervisor whose sockets and pod bundles live under the deleted tree.
    pub(crate) fn discard_fork(&mut self, prepared: &PreparedFork) {
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

    /// Snapshots the project and its world data, then switches the fork checkout to its branch.
    fn snapshot(
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

    /// Stops the fork when live, then removes every fork artifact and its single ledger row.
    pub(crate) fn drop_fork(
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
        WorldError::NotBtrfs { .. } => Failure::new(
            "not-btrfs",
            format!("`{}` is not on a Btrfs filesystem", project.display()),
        ),
        WorldError::NestedMount { .. } => {
            Failure::new("nested-mount", first_line(&error.to_string()))
        }
        WorldError::UnsupportedEntry { .. } => {
            Failure::new("unsupported-entry", first_line(&error.to_string()))
        }
        WorldError::NotASubvolume { path } => Failure::new(
            "not-subvolume",
            format!("`{}` is not a Btrfs subvolume", path.display()),
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

#[cfg(test)]
mod tests {
    use std::fs;

    use slipfork_forkfile::FeatureEdit;

    use super::Core;
    use crate::world::features::WorldFeatures;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    /// A fork boots from the snapshot's inherited file; the request's edits must be persisted
    /// into the fork checkout, not merely validated in memory.
    #[test]
    fn apply_features_persists_the_merged_edits_into_the_fork_checkout() {
        let root = temp("slipfork-fork-features");
        let project = root.path().join("feat-auth");
        fs::create_dir_all(project.join(".slipfork")).expect("create the fork checkout");
        fs::write(
            project.join("Forkfile"),
            "# forkfile: 0.1\n\nfeatures {\n  dev\n  mail\n}\n",
        )
        .expect("write the fork's Forkfile");
        WorldFeatures::new(false, vec![FeatureEdit::Enable("mail".to_owned())])
            .save(&project)
            .expect("write the inherited features file");

        let request = WorldFeatures::new(false, vec![FeatureEdit::Disable("dev".to_owned())]);
        let applied = Core::apply_features(&project, request).expect("apply the fork's features");
        let persisted = WorldFeatures::load(&project).expect("read the persisted file");
        assert_eq!(persisted, applied);
        assert!(
            persisted
                .edits
                .contains(&FeatureEdit::Enable("mail".to_owned())),
            "the inherited edit survives: {persisted:?}"
        );
        assert!(
            persisted
                .edits
                .contains(&FeatureEdit::Disable("dev".to_owned()))
        );
        let text = fs::read_to_string(WorldFeatures::path(&project)).expect("features.json");
        assert!(text.contains("\"+mail\""), "{text}");
        assert!(text.contains("\"-dev\""), "{text}");

        let reset = WorldFeatures::new(true, Vec::new());
        let applied = Core::apply_features(&project, reset).expect("reset the base");
        assert!(applied.no_default);
        assert!(applied.edits.is_empty(), "{applied:?}");
        assert!(
            WorldFeatures::load(&project)
                .expect("read the reset file")
                .no_default,
            "an explicit reset is persisted too"
        );

        let checked = slipfork_forkfile::compile(
            &fs::read_to_string(project.join("Forkfile")).expect("read Forkfile"),
        )
        .expect("compile Forkfile");
        let resolved = WorldFeatures::load_resolved(&project, &checked.document)
            .expect("resolve persisted fork features");
        assert!(
            resolved.active().is_empty(),
            "persisted `--no-default-features` resolves an empty active set: {resolved:?}"
        );

        let inherited_rows = Core::planned_fork_rows(&project, "feat-auth", None);
        assert!(
            !inherited_rows
                .iter()
                .any(|(kind, _)| kind == super::FEATURE_REBUILD_ROW.0),
            "a fork without feature overrides keeps the 0ms plan rows: {inherited_rows:?}"
        );
        let override_req = WorldFeatures::new(false, vec![FeatureEdit::Disable("dev".to_owned())]);
        let override_rows = Core::planned_fork_rows(&project, "feat-auth", Some(&override_req));
        assert!(
            override_rows.contains(&(
                super::FEATURE_REBUILD_ROW.0.to_owned(),
                super::FEATURE_REBUILD_ROW.1.to_owned(),
            )),
            "a feature-overridden fork announces why images rebuild: {override_rows:?}"
        );

        fs::remove_file(project.join("Forkfile")).expect("remove Forkfile");
        let missing = Core::apply_features(&project, WorldFeatures::new(true, Vec::new()))
            .expect_err("missing Forkfile must fail closed");
        assert_eq!(missing.code, "forkfile");
        assert_eq!(
            missing.message,
            "cannot read `Forkfile`\nnext: restore the file, then retry `slipfork fork`"
        );
    }
}
