//! `ConvergeExecutor` (`specs/03` §7.4): settles ledger rows against disk, then re-observes.
//!
//! The daemon feeds the pure `plan_converge` planner with the engine's on-disk facts, applies
//! every returned action here, and re-lists rows until the plan is empty (three rounds per store).
//! No frame is emitted: convergence runs before `up`/`ps` answers, never inside a streamed plan.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::control::{PROTOCOL_VERSION, Request};
use slipfork_world::{
    BtrfsEngine, ConvergeAction, Dirtiness, Error as WorldError, ForkName, ForkRequest,
    LedgerError, RuntimePods, StoreLayout, WorldEngine, WorldName, WorldState, is_subvolume_path,
    kill_group, plan_converge, reap_world_tree,
};

use super::Failure;
use super::lifecycle::core::{Core, World, ledger_failure};
use super::now_ms;
use super::rpc::{key_of, pid_alive, world_answers, world_call_within};

/// Longest one converge control call or process reap may block.
const CONVERGE_BUDGET: Duration = Duration::from_secs(5);
/// Rounds one store gets to settle before the next boot proceeds.
const CONVERGE_ROUNDS: usize = 3;

impl Core {
    /// Settles every ledger row against the store and disk before a boot, `ps`, or startup.
    ///
    /// A world this daemon is still booting is never touched: its `Booting` row is filtered out of
    /// every round, so a boot in flight can never be reaped by its own convergence pass.
    pub(crate) fn converge(&mut self) -> Result<(), Failure> {
        let mut rows = self.ledger.list().map_err(ledger_failure)?;
        // A supervisor survives a daemon restart: every live ledger pid `/proc` still proves is
        // part of the alive set, unioned with the pids this daemon spawned itself.
        let mut alive: BTreeSet<i64> = rows
            .worlds
            .iter()
            .filter_map(|row| row.supervisor_pid)
            .collect();
        alive.extend(self.worlds.values().filter_map(World::pid).map(i64::from));
        alive.retain(|pid| u32::try_from(*pid).is_ok_and(pid_alive));
        let booting: BTreeSet<i64> = self
            .worlds
            .values()
            .filter(|world| matches!(world, World::Booting { .. }))
            .map(|world| world.row().id)
            .collect();
        let stores: BTreeSet<PathBuf> = rows
            .worlds
            .iter()
            .map(|row| row.store_root.clone())
            .collect();
        for store_root in stores {
            let engine = BtrfsEngine::new(StoreLayout::at(store_root));
            for _ in 0..CONVERGE_ROUNDS {
                let input = engine
                    .observe(&rows, &alive, &self.host.runtime_dir)
                    .map_err(|error| {
                        Failure::new(
                            "converge-observe",
                            format!("observe the store: {}", one_line(&error.to_string())),
                        )
                    })?;
                let actions: Vec<ConvergeAction> = plan_converge(&input)
                    .into_iter()
                    .filter(|action| world_id(action).is_none_or(|id| !booting.contains(&id)))
                    .collect();
                if actions.is_empty() {
                    break;
                }
                for action in actions {
                    self.apply(action, &engine)?;
                }
                rows = self.ledger.list().map_err(ledger_failure)?;
            }
        }
        self.adopt_survivors()
    }

    /// Re-adopts a live world whose supervisor and control socket survived a daemon restart.
    ///
    /// `self.worlds` is empty at startup, so without this every live row would be invisible to
    /// `down`/`ps`/`up` until it was reaped. Only a `live` row with an alive supervisor and an
    /// answering pod control socket is adopted; a row this daemon already owns is left untouched.
    fn adopt_survivors(&mut self) -> Result<(), Failure> {
        let rows = self.ledger.list().map_err(ledger_failure)?;
        for row in rows.worlds {
            if row.state != WorldState::Live {
                continue;
            }
            let alive = row
                .supervisor_pid
                .and_then(|pid| u32::try_from(pid).ok())
                .is_some_and(pid_alive);
            let answers = row.pods_dir.as_deref().is_some_and(world_answers);
            if !alive || !answers {
                continue;
            }
            let key = key_of(&row.checkout_path);
            self.worlds
                .entry(key)
                .or_insert(World::Live { row, child: None });
        }
        Ok(())
    }

    /// Applies one planned action; a best-effort teardown never fails the whole convergence pass.
    fn apply(&mut self, action: ConvergeAction, engine: &BtrfsEngine) -> Result<(), Failure> {
        match action {
            ConvergeAction::ReapWorld {
                world_id,
                world_name,
                supervisor_pid,
            } => self.reap_world(world_id, &world_name, supervisor_pid),
            ConvergeAction::SweepPods { pod_dirs, .. } => {
                self.sweep_pod_dirs(&pod_dirs, engine);
            }
            ConvergeAction::UnlinkLease {
                world_id,
                world_name,
            } => self.unlink_lease(world_id, &world_name),
            ConvergeAction::MarkStopped { world_id, .. } => {
                self.ledger
                    .mark_stopped(world_id, now_ms())
                    .map_err(ledger_failure)?;
            }
            ConvergeAction::MarkBroken {
                world_id, problem, ..
            } => {
                self.ledger
                    .mark_broken(world_id, problem, now_ms())
                    .map_err(ledger_failure)?;
            }
            ConvergeAction::DeleteRow { world_id, .. } => {
                self.ledger
                    .delete_world_row(world_id)
                    .map_err(ledger_failure)?;
            }
            ConvergeAction::DeleteUntracked { checkout_path, .. } => {
                self.remove_checkout(&checkout_path, engine);
            }
            ConvergeAction::AdoptFork {
                root_id,
                name,
                checkout_path,
                parent_hint,
                ..
            } => self.adopt_fork(root_id, &name, &checkout_path, parent_hint, engine)?,
            ConvergeAction::DestroyWorldData {
                world_name,
                store_root,
                ..
            } => self.destroy_world_data(&world_name, store_root),
            ConvergeAction::RemoveCheckout { checkout_path, .. } => {
                self.remove_checkout(&checkout_path, engine);
            }
            ConvergeAction::DestroyOrphan { store_root, name } => {
                self.destroy_orphan(store_root, &name);
            }
            ConvergeAction::SweepOrphanPods { pod_dirs, .. } => {
                for dir in &pod_dirs {
                    let shutdown = Request::Shutdown {
                        v: PROTOCOL_VERSION,
                        id: "daemon-converge-shutdown".to_owned(),
                    };
                    let _ = world_call_within(dir, &shutdown, CONVERGE_BUDGET);
                    reap_world_tree(0, Some(dir), CONVERGE_BUDGET);
                }
                self.sweep_pod_dirs(&pod_dirs, engine);
            }
        }
        Ok(())
    }

    /// Kills one world's `-pgid` tree and anything carrying its `--pods` argv.
    fn reap_world(&self, world_id: i64, world_name: &str, supervisor_pid: Option<i64>) {
        let runtime = WorldName::parse(world_name)
            .ok()
            .map(|world| RuntimePods::world_root(&self.host.runtime_dir, &world));
        let pods = self
            .row_by_id(world_id)
            .and_then(|row| row.pods_dir)
            .or(runtime);
        match supervisor_pid {
            Some(pid) => {
                if let Ok(pid) = i32::try_from(pid) {
                    kill_group(pid);
                }
                reap_world_tree(
                    u32::try_from(pid).unwrap_or(0),
                    pods.as_deref(),
                    CONVERGE_BUDGET,
                );
            }
            None => reap_world_tree(0, pods.as_deref(), CONVERGE_BUDGET),
        }
    }

    /// Removes an unreadable lease so `gc` is unblocked; a missing one is already settled.
    fn unlink_lease(&self, world_id: i64, world_name: &str) {
        let Some(store_root) = self.row_by_id(world_id).map(|row| row.store_root) else {
            return;
        };
        let Ok(world) = WorldName::parse(world_name) else {
            return;
        };
        match fs::remove_file(super::lease::path(&store_root, &world)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => Self::note(&format!("unlink the lease of `{world_name}`: {error}")),
        }
    }

    /// Registers a real untracked `.forks/<name>` checkout as a stopped fork row.
    fn adopt_fork(
        &mut self,
        root_id: i64,
        name: &str,
        checkout_path: &Path,
        parent_hint: Option<String>,
        engine: &BtrfsEngine,
    ) -> Result<(), Failure> {
        // The lease's `origin` names the real parent; a hint with no row is reported instead of
        // silently flattening the fork onto the root.
        let rows = self.ledger.list().map_err(ledger_failure)?;
        let parent_id = match parent_hint {
            Some(hint) => rows
                .worlds
                .iter()
                .find(|row| row.world_name == hint)
                .map_or_else(
                    || {
                        Self::note(&format!(
                            "adopting `{name}` under its root: parent `{hint}` has no ledger row"
                        ));
                        root_id
                    },
                    |row| row.id,
                ),
            None => root_id,
        };
        let Ok(fork) = ForkName::parse(name) else {
            Self::note(&format!("cannot adopt `{name}`: invalid fork name"));
            self.remove_checkout(checkout_path, engine);
            return Ok(());
        };
        let request = ForkRequest {
            root_id,
            parent_id,
            name: fork,
            runtime_dir: self.host.runtime_dir.clone(),
            branch: String::new(),
            now_ms: now_ms(),
        };
        match self.ledger.begin_fork(request) {
            Ok(row) => {
                self.ledger
                    .mark_stopped(row.world_id, now_ms())
                    .map_err(ledger_failure)?;
            }
            Err(LedgerError::NameTooLong { .. } | LedgerError::NameCollision { .. }) => {
                Self::note(&format!(
                    "cannot adopt `{name}`: the ledger refused the fork name"
                ));
                self.remove_checkout(checkout_path, engine);
            }
            Err(error) => return Err(ledger_failure(error)),
        }
        Ok(())
    }

    /// Destroys a world's data subvolume, its flat clones, and its tmpfs pod root.
    fn destroy_world_data(&self, world_name: &str, store_root: PathBuf) {
        let Ok(world) = WorldName::parse(world_name) else {
            return;
        };
        self.drop_world_store(&world, store_root);
    }

    /// Destroys every subvolume a vanished world left behind, plus its tmpfs pod root.
    fn destroy_orphan(&self, store_root: PathBuf, name: &str) {
        let Ok(world) = WorldName::parse(name) else {
            Self::note(&format!(
                "cannot destroy orphan `{name}`: invalid world name"
            ));
            return;
        };
        self.drop_world_store(&world, store_root);
    }

    /// The one store sweep: `drop_world` owns the layout, so no caller re-walks `worlds/`.
    fn drop_world_store(&self, world: &WorldName, store_root: PathBuf) {
        let engine = BtrfsEngine::new(StoreLayout::at(store_root));
        match engine.drop_world(world) {
            Ok(()) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => Self::note(&format!(
                "destroy the data of `{world}`: {}",
                one_line(&error.to_string())
            )),
        }
        if let Err(error) = RuntimePods::purge(&self.host.runtime_dir, world) {
            Self::note(&format!("purge the pods of `{world}`: {error}"));
        }
    }

    /// Sweeps every observed pod directory: tmpfs roots by `remove_dir_all`, store roots by engine.
    fn sweep_pod_dirs(&self, pod_dirs: &[PathBuf], engine: &BtrfsEngine) {
        let runtime_root = RuntimePods::worlds_root(&self.host.runtime_dir);
        for dir in pod_dirs {
            let outcome = if dir.starts_with(&runtime_root) {
                match fs::remove_dir_all(dir) {
                    Ok(()) => None,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => Some(error.to_string()),
                }
            } else {
                match engine.sweep_pods(dir) {
                    Ok(()) => None,
                    Err(error) if is_not_found(&error) => None,
                    Err(error) => Some(one_line(&error.to_string())),
                }
            };
            if let Some(message) = outcome {
                Self::note(&format!("sweep `{}`: {message}", dir.display()));
            }
        }
    }

    /// Removes a fork checkout: a Btrfs subvolume through the engine, else a plain directory.
    fn remove_checkout(&self, path: &Path, engine: &BtrfsEngine) {
        if is_subvolume_path(path).unwrap_or(false) {
            match engine.destroy(path, Dirtiness::Clean, true) {
                Ok(()) => {}
                Err(error) if is_not_found(&error) => {}
                Err(error) => Self::note(&format!(
                    "remove `{}`: {}",
                    path.display(),
                    one_line(&error.to_string())
                )),
            }
            return;
        }
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => Self::note(&format!("remove `{}`: {error}", path.display())),
        }
    }

    /// One stderr line naming the convergence step that could not finish.
    fn note(message: &str) {
        eprintln!("slipfork daemon: converge: {message}");
    }
}

/// The row id an action settles, so a world this daemon is still booting is never touched.
fn world_id(action: &ConvergeAction) -> Option<i64> {
    match action {
        ConvergeAction::ReapWorld { world_id, .. }
        | ConvergeAction::SweepPods { world_id, .. }
        | ConvergeAction::UnlinkLease { world_id, .. }
        | ConvergeAction::MarkStopped { world_id, .. }
        | ConvergeAction::MarkBroken { world_id, .. }
        | ConvergeAction::DestroyWorldData { world_id, .. }
        | ConvergeAction::RemoveCheckout { world_id, .. }
        | ConvergeAction::DeleteRow { world_id, .. } => Some(*world_id),
        ConvergeAction::DeleteUntracked { .. }
        | ConvergeAction::AdoptFork { .. }
        | ConvergeAction::DestroyOrphan { .. }
        | ConvergeAction::SweepOrphanPods { .. } => None,
    }
}

fn is_not_found(error: &WorldError) -> bool {
    matches!(error, WorldError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
}

fn one_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}
