//! Pure convergence planner matrix (`specs/03` §7.4): rows plus observed facts in, ordered
//! actions out. No database, no disk, no clock.

use std::collections::BTreeMap;
use std::path::PathBuf;

use slipfork_world::ledger::{
    ConvergeAction, ConvergeInput, ForkRow, LeaseState, OrphanWorld, UnregisteredFork,
    UntrackedKind, WorldFacts, WorldRow, WorldState, plan_converge,
};

macro_rules! has {
    ($plan:expr, $pattern:pat) => {
        $plan.iter().any(|action| matches!(action, $pattern))
    };
    ($plan:expr, $pattern:pat if $guard:expr) => {
        $plan.iter().any(|action| matches!(action, $pattern if $guard))
    };
}

const STORE: &str = "/var/tmp/slipfork-ledger-store";

fn wrow(id: i64, name: &str, state: WorldState) -> WorldRow {
    WorldRow {
        id,
        checkout_path: PathBuf::from(format!("/var/tmp/proj/{name}")),
        world_name: name.to_owned(),
        store_root: PathBuf::from(STORE),
        state,
        supervisor_pid: None,
        pods_dir: None,
        started_at_ms: None,
        problem: None,
        updated_at_ms: 0,
    }
}

fn frow(world_id: i64, root_id: i64, parent_id: i64, name: &str) -> ForkRow {
    ForkRow {
        world_id,
        root_id,
        parent_id,
        name: name.to_owned(),
        branch: "b".to_owned(),
    }
}

fn facts(
    checkout: bool,
    data: bool,
    alive: bool,
    lease: LeaseState,
    pods: Vec<PathBuf>,
) -> WorldFacts {
    WorldFacts {
        checkout_exists: checkout,
        world_data_exists: data,
        supervisor_alive: alive,
        lease,
        pod_dirs: pods,
    }
}

fn pod(world: &str) -> PathBuf {
    PathBuf::from(format!("/run/user/1000/slipfork/pods/{world}"))
}

fn input() -> ConvergeInput {
    ConvergeInput {
        worlds: vec![
            WorldRow {
                supervisor_pid: Some(42),
                ..wrow(1, "test1", WorldState::Live)
            },
            wrow(2, "fork2", WorldState::Live),
            wrow(3, "fork3", WorldState::Stopped),
            wrow(4, "fork4", WorldState::Creating),
            wrow(5, "fork5", WorldState::Broken),
            wrow(6, "root-lost", WorldState::Live),
            wrow(7, "fork7", WorldState::Booting),
            wrow(8, "fork8", WorldState::Stopping),
            wrow(9, "fork9", WorldState::Deleting),
            wrow(10, "fork10", WorldState::Live),
            wrow(11, "root-creating", WorldState::Creating),
            wrow(12, "fork12", WorldState::Live),
            wrow(13, "fork13", WorldState::Stopped),
        ],
        forks: vec![
            frow(2, 1, 1, "fork2"),
            frow(3, 1, 1, "fork3"),
            frow(4, 1, 1, "fork4"),
            frow(5, 1, 1, "fork5"),
            frow(7, 1, 1, "fork7"),
            frow(8, 1, 1, "fork8"),
            frow(9, 1, 1, "fork9"),
            frow(10, 1, 1, "fork10"),
            frow(12, 1, 1, "fork12"),
            frow(13, 1, 1, "fork13"),
        ],
        facts: [
            (1, facts(true, true, true, LeaseState::Present, vec![])),
            (
                2,
                facts(true, true, false, LeaseState::Present, vec![pod("fork2")]),
            ),
            (
                3,
                facts(true, true, false, LeaseState::Absent, vec![pod("fork3")]),
            ),
            (4, facts(true, true, true, LeaseState::Absent, vec![])),
            (5, facts(true, true, false, LeaseState::Absent, vec![])),
            (6, facts(false, true, true, LeaseState::Present, vec![])),
            (7, facts(true, true, true, LeaseState::Absent, vec![])),
            (8, facts(true, true, true, LeaseState::Absent, vec![])),
            (9, facts(true, true, true, LeaseState::Absent, vec![])),
            (
                10,
                facts(true, true, true, LeaseState::Corrupt, vec![pod("fork10")]),
            ),
            (11, facts(false, true, true, LeaseState::Absent, vec![])),
            (12, facts(false, true, true, LeaseState::Present, vec![])),
            (13, facts(false, false, false, LeaseState::Absent, vec![])),
        ]
        .into_iter()
        .collect(),
        untracked: vec![
            UnregisteredFork {
                root_id: 1,
                name: "stub".to_owned(),
                checkout_path: PathBuf::from("/var/tmp/proj/test1/.forks/stub"),
                kind: UntrackedKind::Stub,
            },
            UnregisteredFork {
                root_id: 1,
                name: "stage".to_owned(),
                checkout_path: PathBuf::from("/var/tmp/proj/test1/.forks/.partial-stage"),
                kind: UntrackedKind::Partial,
            },
            UnregisteredFork {
                root_id: 1,
                name: "empty".to_owned(),
                checkout_path: PathBuf::from("/var/tmp/proj/test1/.forks/empty"),
                kind: UntrackedKind::Checkout {
                    has_forkfile: false,
                    parent_hint: None,
                },
            },
            UnregisteredFork {
                root_id: 1,
                name: "ghost".to_owned(),
                checkout_path: PathBuf::from("/var/tmp/proj/test1/.forks/ghost"),
                kind: UntrackedKind::Checkout {
                    has_forkfile: true,
                    parent_hint: Some("fork2".to_owned()),
                },
            },
        ],
        orphan_worlds: vec![OrphanWorld {
            store_root: PathBuf::from(STORE),
            name: "ghost--test1".to_owned(),
        }],
    }
}

/// Applies every action to a copy of the input, modelling the executor's post-action state.
fn settle(mut input: ConvergeInput) -> ConvergeInput {
    let plan = plan_converge(&input);
    let mut deleted: Vec<i64> = Vec::new();
    for action in &plan {
        match action {
            ConvergeAction::ReapWorld { world_id, .. } => {
                if let Some(world) = input.worlds.iter_mut().find(|world| world.id == *world_id) {
                    world.supervisor_pid = None;
                }
                input.facts.entry(*world_id).or_default().supervisor_alive = false;
            }
            ConvergeAction::SweepPods { world_id, .. } => {
                input.facts.entry(*world_id).or_default().pod_dirs.clear();
            }
            ConvergeAction::UnlinkLease { world_id, .. } => {
                input.facts.entry(*world_id).or_default().lease = LeaseState::Absent;
            }
            ConvergeAction::MarkStopped { world_id, .. } => {
                if let Some(world) = input.worlds.iter_mut().find(|world| world.id == *world_id) {
                    world.state = WorldState::Stopped;
                    world.supervisor_pid = None;
                    world.problem = None;
                }
            }
            ConvergeAction::MarkBroken {
                world_id, problem, ..
            } => {
                if let Some(world) = input.worlds.iter_mut().find(|world| world.id == *world_id) {
                    world.state = WorldState::Broken;
                    world.supervisor_pid = None;
                    world.problem = Some(problem.clone());
                }
            }
            ConvergeAction::DestroyWorldData { world_id, .. } => {
                input.facts.entry(*world_id).or_default().world_data_exists = false;
            }
            ConvergeAction::RemoveCheckout { world_id, .. } => {
                input.facts.entry(*world_id).or_default().checkout_exists = false;
            }
            ConvergeAction::DeleteRow { world_id, .. } => deleted.push(*world_id),
            ConvergeAction::DeleteUntracked { name, .. }
            | ConvergeAction::AdoptFork { name, .. } => {
                input.untracked.retain(|fork| fork.name != *name);
            }
            ConvergeAction::DestroyOrphan { .. } => {}
        }
    }
    input.worlds.retain(|world| !deleted.contains(&world.id));
    input.forks.retain(|fork| !deleted.contains(&fork.world_id));
    for id in deleted {
        input.facts.remove(&id);
    }
    input.orphan_worlds.clear();
    input
}

#[test]
fn plan_covers_every_divergence_in_order() {
    let plan = plan_converge(&input());

    assert!(
        has!(plan, ConvergeAction::ReapWorld { world_id: 2, .. }),
        "dead live"
    );
    assert!(has!(plan, ConvergeAction::SweepPods { world_id: 2, .. }));
    assert!(has!(plan, ConvergeAction::MarkStopped { world_id: 2, .. }));
    assert!(
        has!(plan, ConvergeAction::SweepPods { world_id: 3, .. }),
        "leaked stopped pods"
    );
    assert!(
        has!(plan, ConvergeAction::MarkStopped { world_id: 5, .. }),
        "restored checkout"
    );
    assert!(
        has!(plan, ConvergeAction::ReapWorld { world_id: 7, .. }),
        "booting reaped"
    );
    assert!(has!(plan, ConvergeAction::MarkStopped { world_id: 7, .. }));
    assert!(
        has!(plan, ConvergeAction::MarkStopped { world_id: 8, .. }),
        "stopping reaped"
    );
    assert!(
        has!(plan, ConvergeAction::DeleteRow { world_id: 9, .. }),
        "deleting rolls back"
    );
    assert!(
        has!(plan, ConvergeAction::UnlinkLease { world_id: 10, .. }),
        "corrupt lease"
    );
    assert!(has!(plan, ConvergeAction::MarkStopped { world_id: 10, .. }));
    assert!(
        has!(plan, ConvergeAction::MarkBroken { world_id: 6, .. }),
        "root stays broken"
    );
    assert!(
        !has!(plan, ConvergeAction::DestroyWorldData { world_id: 6, .. }),
        "root data kept"
    );
    assert!(
        !has!(plan, ConvergeAction::RemoveCheckout { world_id: 6, .. }),
        "root checkout kept"
    );
    assert!(
        has!(plan, ConvergeAction::MarkBroken { world_id: 11, .. }),
        "creating root"
    );
    assert!(
        has!(plan, ConvergeAction::MarkBroken { world_id: 12, remedy, .. } if remedy.contains("slipfork drop")),
        "fork missing checkout with data"
    );
    assert!(has!(
        plan,
        ConvergeAction::DestroyWorldData { world_id: 4, .. }
    ));
    assert!(has!(
        plan,
        ConvergeAction::RemoveCheckout { world_id: 4, .. }
    ));
    assert!(has!(plan, ConvergeAction::DeleteRow { world_id: 4, .. }));
    assert!(
        has!(plan, ConvergeAction::DeleteRow { world_id: 13, .. }),
        "missing checkout and data"
    );
    assert!(has!(plan, ConvergeAction::DeleteUntracked { name, .. } if name == "stub"));
    assert!(has!(plan, ConvergeAction::DeleteUntracked { name, .. } if name == "stage"));
    assert!(has!(plan, ConvergeAction::DeleteUntracked { name, .. } if name == "empty"));
    assert!(
        has!(plan, ConvergeAction::AdoptFork { name, branch, .. } if name == "ghost" && branch.is_empty())
    );
    assert!(has!(plan, ConvergeAction::DestroyOrphan { name, .. } if name == "ghost--test1"));

    let position =
        |wanted: fn(&ConvergeAction) -> bool| plan.iter().position(wanted).expect("action");
    let reap = position(|action| matches!(action, ConvergeAction::ReapWorld { world_id: 4, .. }));
    let delete = position(|action| matches!(action, ConvergeAction::DeleteRow { world_id: 4, .. }));
    assert!(reap < delete, "reap before delete");
    let destroy =
        position(|action| matches!(action, ConvergeAction::DestroyWorldData { world_id: 4, .. }));
    assert!(destroy < delete, "destroy before delete");
}

#[test]
fn settled_plan_is_empty() {
    let settled = settle(input());
    assert!(
        plan_converge(&settled).is_empty(),
        "{:?}",
        plan_converge(&settled)
    );
    let rows: BTreeMap<i64, WorldState> = settled
        .worlds
        .iter()
        .map(|world| (world.id, world.state.clone()))
        .collect();
    assert_eq!(rows.get(&2), Some(&WorldState::Stopped));
    assert_eq!(rows.get(&7), Some(&WorldState::Stopped));
    assert_eq!(rows.get(&10), Some(&WorldState::Stopped));
    assert_eq!(rows.get(&6), Some(&WorldState::Broken));
    assert_eq!(rows.get(&12), Some(&WorldState::Broken));
    assert!(!rows.contains_key(&4));
    assert!(!rows.contains_key(&9));
    assert!(!rows.contains_key(&13));
    assert!(settled.untracked.is_empty());
    assert!(settled.orphan_worlds.is_empty());
}
