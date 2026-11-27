//! Pure convergence planner (`specs/03` §7.4): rows plus observed facts in, ordered actions out.
//!
//! This module performs no I/O, reads no clock, and never touches disk; the `ConvergeExecutor`
//! in the daemon applies the plan and feeds the post-action facts back for idempotence.

mod input;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub use input::{
    ConvergeInput, ForkRow, LeaseState, LedgerRows, OrphanPods, OrphanWorld, UnregisteredFork,
    UntrackedKind, WorldFacts, WorldRow,
};

use crate::ledger::entity::WorldState;

/// One ordered step for the `ConvergeExecutor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvergeAction {
    /// Kill the supervisor's `-pgid` tree.
    ReapWorld {
        /// Row id.
        world_id: i64,
        /// `<fork>--<root>` or root name.
        world_name: String,
        /// Recorded leader, when present.
        supervisor_pid: Option<i64>,
    },
    /// Remove `<runtime_dir>/pods/<world>` and `<store>/pods/<world>`.
    SweepPods {
        /// Row id.
        world_id: i64,
        /// World id the pods belong to.
        world_name: String,
        /// The directories observed.
        pod_dirs: Vec<PathBuf>,
    },
    /// Unlink an unreadable `.slipfork-lease.json` so `gc` is unblocked.
    UnlinkLease {
        /// Row id.
        world_id: i64,
        /// World holding the lease.
        world_name: String,
    },
    /// Settle the row at `stopped`, clearing supervisor/problem.
    MarkStopped {
        /// Row id.
        world_id: i64,
        /// World name.
        world_name: String,
    },
    /// Settle the row at `broken` with a one-line problem and a `next:` remedy.
    MarkBroken {
        /// Row id.
        world_id: i64,
        /// World name.
        world_name: String,
        /// One-line divergence.
        problem: String,
        /// The user's next action.
        remedy: String,
    },
    /// Remove a stub or `.partial-*` stage that has no row.
    DeleteUntracked {
        /// Family root id.
        root_id: i64,
        /// The `.forks/<name>` entry.
        name: String,
        /// Its absolute path.
        checkout_path: PathBuf,
    },
    /// Register a real untracked checkout as `stopped`.
    AdoptFork {
        /// Family root id.
        root_id: i64,
        /// The `.forks/<name>` entry.
        name: String,
        /// Its absolute path.
        checkout_path: PathBuf,
        /// Always empty; `up` fills it.
        branch: String,
        /// Parent recovered from `Lease.origin`, else `None` for the root.
        parent_hint: Option<String>,
    },
    /// Destroy `worlds/<world>` and its flat `.kind.pod` clones.
    DestroyWorldData {
        /// Row id.
        world_id: i64,
        /// World id the data belongs to.
        world_name: String,
        /// The store holding the subvolumes.
        store_root: PathBuf,
    },
    /// Remove the fork's `checkout_path`.
    RemoveCheckout {
        /// Row id.
        world_id: i64,
        /// World name.
        world_name: String,
        /// The checkout to remove.
        checkout_path: PathBuf,
    },
    /// Delete the row through `delete_world_row` (splicing children).
    DeleteRow {
        /// Row id.
        world_id: i64,
        /// World name.
        world_name: String,
    },
    /// Destroy `worlds/<fork>--<root>*` and `<store>/pods/<fork>--<root>`.
    DestroyOrphan {
        /// The store.
        store_root: PathBuf,
        /// The orphan subvolume prefix.
        name: String,
    },
    /// Shutdown if `control.sock` answers, reap the `-pgid`/`world_init`/`slirp4netns` tree,
    /// then remove both pod directories.
    SweepOrphanPods {
        /// The store the world belongs to.
        store_root: PathBuf,
        /// The world id the pod directories belong to.
        world_name: String,
        /// The observed pod directories.
        pod_dirs: Vec<PathBuf>,
    },
}

/// Plans every §7.4 divergence deterministically; a settled ledger yields an empty plan.
#[must_use]
pub fn plan_converge(input: &ConvergeInput) -> Vec<ConvergeAction> {
    let forks: BTreeMap<i64, &ForkRow> = input
        .forks
        .iter()
        .map(|fork| (fork.world_id, fork))
        .collect();
    let mut worlds: Vec<&WorldRow> = input.worlds.iter().collect();
    worlds.sort_by_key(|world| world.id);

    let mut actions = Vec::new();
    for world in worlds {
        let facts = input.facts.get(&world.id).cloned().unwrap_or_default();
        plan_world(world, forks.contains_key(&world.id), &facts, &mut actions);
    }

    let mut untracked: Vec<&UnregisteredFork> = input.untracked.iter().collect();
    untracked.sort_by(|left, right| {
        (left.root_id, left.name.as_str()).cmp(&(right.root_id, right.name.as_str()))
    });
    for fork in untracked {
        actions.push(untracked_action(fork));
    }

    let mut orphans: Vec<&OrphanWorld> = input.orphan_worlds.iter().collect();
    orphans.sort_by(|left, right| {
        (left.store_root.as_path(), left.name.as_str())
            .cmp(&(right.store_root.as_path(), right.name.as_str()))
    });
    for orphan in orphans {
        actions.push(ConvergeAction::DestroyOrphan {
            store_root: orphan.store_root.clone(),
            name: orphan.name.clone(),
        });
    }

    // A pod directory belonging to a known row is owned by that row's facts, never swept here.
    let known: BTreeSet<&str> = input
        .worlds
        .iter()
        .map(|world| world.world_name.as_str())
        .collect();
    let mut orphan_pods: Vec<&OrphanPods> = input
        .orphan_pods
        .iter()
        .filter(|pods| !pods.pod_dirs.is_empty() && !known.contains(pods.world_name.as_str()))
        .collect();
    orphan_pods.sort_by(|left, right| {
        (left.store_root.as_path(), left.world_name.as_str())
            .cmp(&(right.store_root.as_path(), right.world_name.as_str()))
    });
    for pods in orphan_pods {
        let mut pod_dirs = pods.pod_dirs.clone();
        pod_dirs.sort();
        pod_dirs.dedup();
        if pod_dirs.is_empty() {
            continue;
        }
        actions.push(ConvergeAction::SweepOrphanPods {
            store_root: pods.store_root.clone(),
            world_name: pods.world_name.clone(),
            pod_dirs,
        });
    }
    actions
}

fn plan_world(world: &WorldRow, is_fork: bool, facts: &WorldFacts, out: &mut Vec<ConvergeAction>) {
    match world.state {
        WorldState::Creating | WorldState::Deleting => rollback(world, is_fork, facts, out),
        WorldState::Booting | WorldState::Stopping => {
            reap(world, out);
            sweep_pods(world, facts, out);
            mark_stopped(world, out);
        }
        WorldState::Live => {
            if !facts.checkout_exists {
                missing_checkout(world, is_fork, facts, out);
            } else if world.supervisor_pid.is_none()
                || !facts.supervisor_alive
                || facts.lease != LeaseState::Present
            {
                reap(world, out);
                sweep_pods(world, facts, out);
                if facts.lease == LeaseState::Corrupt {
                    out.push(ConvergeAction::UnlinkLease {
                        world_id: world.id,
                        world_name: world.world_name.clone(),
                    });
                }
                mark_stopped(world, out);
            }
        }
        WorldState::Stopped => {
            if !facts.checkout_exists {
                missing_checkout(world, is_fork, facts, out);
            } else {
                if world.supervisor_pid.is_some() || facts.supervisor_alive {
                    reap(world, out);
                }
                sweep_pods(world, facts, out);
                if world.supervisor_pid.is_some() {
                    mark_stopped(world, out);
                }
            }
        }
        WorldState::Broken => {
            if !facts.checkout_exists {
                sweep_pods(world, facts, out);
                if is_fork && !facts.world_data_exists {
                    destroy(world, facts, out);
                    remove_checkout(world, out);
                    delete_row(world, out);
                }
            } else {
                mark_stopped(world, out);
            }
        }
    }
}

fn rollback(world: &WorldRow, is_fork: bool, facts: &WorldFacts, out: &mut Vec<ConvergeAction>) {
    reap(world, out);
    sweep_pods(world, facts, out);
    if is_fork {
        destroy(world, facts, out);
        remove_checkout(world, out);
        delete_row(world, out);
    } else {
        out.push(ConvergeAction::MarkBroken {
            world_id: world.id,
            world_name: world.world_name.clone(),
            problem: "root world cannot be rolled back or deleted".to_owned(),
            remedy: format!(
                "restore {} or run `slipfork down {}`",
                world.checkout_path.display(),
                world.world_name
            ),
        });
    }
}

fn missing_checkout(
    world: &WorldRow,
    is_fork: bool,
    facts: &WorldFacts,
    out: &mut Vec<ConvergeAction>,
) {
    reap(world, out);
    sweep_pods(world, facts, out);
    if is_fork && !facts.world_data_exists {
        destroy(world, facts, out);
        remove_checkout(world, out);
        delete_row(world, out);
        return;
    }
    let remedy = if is_fork {
        format!("slipfork drop {}", world.world_name)
    } else {
        format!(
            "restore {} or run `slipfork down {}`",
            world.checkout_path.display(),
            world.world_name
        )
    };
    out.push(ConvergeAction::MarkBroken {
        world_id: world.id,
        world_name: world.world_name.clone(),
        problem: format!("checkout `{}` is missing", world.checkout_path.display()),
        remedy,
    });
}

fn reap(world: &WorldRow, out: &mut Vec<ConvergeAction>) {
    out.push(ConvergeAction::ReapWorld {
        world_id: world.id,
        world_name: world.world_name.clone(),
        supervisor_pid: world.supervisor_pid,
    });
}

fn sweep_pods(world: &WorldRow, facts: &WorldFacts, out: &mut Vec<ConvergeAction>) {
    if facts.pod_dirs.is_empty() {
        return;
    }
    let mut pod_dirs = facts.pod_dirs.clone();
    pod_dirs.sort();
    out.push(ConvergeAction::SweepPods {
        world_id: world.id,
        world_name: world.world_name.clone(),
        pod_dirs,
    });
}

fn mark_stopped(world: &WorldRow, out: &mut Vec<ConvergeAction>) {
    out.push(ConvergeAction::MarkStopped {
        world_id: world.id,
        world_name: world.world_name.clone(),
    });
}

fn destroy(world: &WorldRow, facts: &WorldFacts, out: &mut Vec<ConvergeAction>) {
    if !facts.world_data_exists {
        return;
    }
    out.push(ConvergeAction::DestroyWorldData {
        world_id: world.id,
        world_name: world.world_name.clone(),
        store_root: world.store_root.clone(),
    });
}

fn remove_checkout(world: &WorldRow, out: &mut Vec<ConvergeAction>) {
    out.push(ConvergeAction::RemoveCheckout {
        world_id: world.id,
        world_name: world.world_name.clone(),
        checkout_path: world.checkout_path.clone(),
    });
}

fn delete_row(world: &WorldRow, out: &mut Vec<ConvergeAction>) {
    out.push(ConvergeAction::DeleteRow {
        world_id: world.id,
        world_name: world.world_name.clone(),
    });
}

fn untracked_action(fork: &UnregisteredFork) -> ConvergeAction {
    match &fork.kind {
        UntrackedKind::Stub | UntrackedKind::Partial => ConvergeAction::DeleteUntracked {
            root_id: fork.root_id,
            name: fork.name.clone(),
            checkout_path: fork.checkout_path.clone(),
        },
        UntrackedKind::Checkout {
            has_forkfile: false,
            ..
        } => ConvergeAction::DeleteUntracked {
            root_id: fork.root_id,
            name: fork.name.clone(),
            checkout_path: fork.checkout_path.clone(),
        },
        UntrackedKind::Checkout {
            has_forkfile: true,
            parent_hint,
        } => ConvergeAction::AdoptFork {
            root_id: fork.root_id,
            name: fork.name.clone(),
            checkout_path: fork.checkout_path.clone(),
            branch: String::new(),
            parent_hint: parent_hint.clone(),
        },
    }
}
