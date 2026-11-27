//! Pure planner matrix plus real-disk observation for the orphan-pods row (`specs/03` §7.4):
//! rows plus observed facts in, ordered actions out. No database, no clock.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use slipfork_world::ledger::{
    ConvergeAction, ConvergeInput, ForkRow, LeaseState, OrphanWorld, UnregisteredFork,
    UntrackedKind, WorldFacts, WorldRow, WorldState, plan_converge,
};
use slipfork_world::{BtrfsEngine, Lease, NodeKey, StoreLayout};

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
        orphan_pods: Vec::new(),
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
            ConvergeAction::SweepOrphanPods { world_name, .. } => {
                input
                    .orphan_pods
                    .retain(|pods| pods.world_name != *world_name);
            }
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

/// §7.4 row 6: ephemeral pods of a world the ledger has no row for are swept exactly once, while a
/// known live row's own pods are owned by its facts and never touched.
#[test]
fn orphan_pods_sweep_is_idempotent_and_spares_known_rows() {
    let tmp = tempfile::Builder::new()
        .prefix("slipfork-ledger-orphan-pods")
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory");
    let store = tmp.path().join(".slipfork-store");
    let runtime = tmp.path().join("runtime");
    let engine = BtrfsEngine::new(StoreLayout::at(store.clone()));

    let root_checkout = tmp.path().join("root");
    fs::create_dir_all(&root_checkout).expect("create the root checkout");
    let live_checkout = tmp.path().join(".forks/live");
    fs::create_dir_all(&live_checkout).expect("create the live fork checkout");

    // A ghost world owns pods on both sides but has no ledger row.
    let ghost_runtime = runtime.join("slipfork/pods/ghost/web");
    let ghost_store = store.join("pods/ghost/web");
    fs::create_dir_all(&ghost_runtime).expect("plant the runtime ghost pods");
    fs::create_dir_all(&ghost_store).expect("plant the store ghost pods");

    // A known live fork with a real lease and real runtime pods.
    let live_world = store.join("worlds/live--root");
    fs::create_dir_all(&live_world).expect("create the live world data");
    let lease = Lease {
        v: 1,
        world: "live--root".to_owned(),
        origin: "root".to_owned(),
        world_epoch: "epoch".to_owned(),
        spec: "spec".to_owned(),
        head: NodeKey::base("amd64", "sha256:live", "linux/amd64"),
        seeds: Vec::new(),
        services: BTreeMap::new(),
        effects: BTreeMap::new(),
    };
    lease
        .write_atomic(&live_world.join(".slipfork-lease.json"))
        .expect("write the live lease");
    let live_runtime = runtime.join("slipfork/pods/live--root");
    fs::create_dir_all(live_runtime.join("web")).expect("plant the live runtime pods");

    // A world owned by another store on the same per-user runtime root: never this store's orphan.
    let other_store = tmp.path().join("other-store");
    let other_checkout = tmp.path().join("other-root");
    fs::create_dir_all(&other_checkout).expect("create the other store checkout");
    let other_world = other_store.join("worlds/other--root");
    fs::create_dir_all(&other_world).expect("create the other store world data");
    let other_lease = Lease {
        v: 1,
        world: "other--root".to_owned(),
        origin: "other--root".to_owned(),
        world_epoch: "epoch".to_owned(),
        spec: "spec".to_owned(),
        head: NodeKey::base("amd64", "sha256:other", "linux/amd64"),
        seeds: Vec::new(),
        services: BTreeMap::new(),
        effects: BTreeMap::new(),
    };
    other_lease
        .write_atomic(&other_world.join(".slipfork-lease.json"))
        .expect("write the other store lease");
    let other_runtime = runtime.join("slipfork/pods/other--root");
    fs::create_dir_all(other_runtime.join("web")).expect("plant the other runtime pods");
    fs::create_dir_all(other_store.join("pods/other--root/web"))
        .expect("plant the other store pods");

    let root_row = WorldRow {
        id: 1,
        checkout_path: root_checkout,
        world_name: "root".to_owned(),
        store_root: store.clone(),
        state: WorldState::Stopped,
        supervisor_pid: None,
        pods_dir: None,
        started_at_ms: None,
        problem: None,
        updated_at_ms: 0,
    };
    let live_row = WorldRow {
        id: 2,
        checkout_path: live_checkout,
        world_name: "live--root".to_owned(),
        store_root: store.clone(),
        state: WorldState::Live,
        supervisor_pid: Some(4242),
        pods_dir: None,
        started_at_ms: Some(1),
        problem: None,
        updated_at_ms: 1,
    };
    let other_row = WorldRow {
        id: 3,
        checkout_path: other_checkout,
        world_name: "other--root".to_owned(),
        store_root: other_store,
        state: WorldState::Live,
        supervisor_pid: Some(4242),
        pods_dir: None,
        started_at_ms: Some(1),
        problem: None,
        updated_at_ms: 1,
    };
    let rows = ledger_rows(vec![root_row, live_row, other_row]);
    let alive = BTreeSet::from([4242_i64]);

    let observed = engine
        .observe(&rows, &alive, &runtime)
        .expect("observe the store");
    assert!(
        observed.facts[&2].pod_dirs.contains(&live_runtime),
        "a live row with a NULL pods_dir still owns its canonical runtime pods"
    );
    let first = plan_converge(&observed);
    let sweeps: Vec<&ConvergeAction> = first
        .iter()
        .filter(|action| matches!(action, ConvergeAction::SweepOrphanPods { .. }))
        .collect();
    assert_eq!(sweeps.len(), 1, "exactly one orphan sweep: {first:?}");
    let ConvergeAction::SweepOrphanPods {
        store_root,
        world_name,
        pod_dirs,
    } = sweeps[0]
    else {
        unreachable!("filtered above")
    };
    assert_eq!(store_root, &store, "the action names the owning store");
    assert_eq!(world_name, "ghost");
    let mut expected = vec![
        store.join("pods/ghost"),
        runtime.join("slipfork/pods/ghost"),
    ];
    expected.sort();
    assert_eq!(pod_dirs, &expected, "both pod roots, sorted");
    assert!(
        !first
            .iter()
            .any(|action| matches!(action, ConvergeAction::SweepPods { world_id: 2, .. })),
        "a known live row's pods are never swept: {first:?}"
    );
    assert!(
        !first.iter().any(|action| matches!(
            action,
            ConvergeAction::SweepOrphanPods { world_name, .. } if world_name == "live--root"
        )),
        "a known live row is never an orphan: {first:?}"
    );
    assert!(
        !first.iter().any(|action| matches!(
            action,
            ConvergeAction::SweepOrphanPods { world_name, .. } if world_name == "other--root"
        )),
        "another store's live world is known and never swept: {first:?}"
    );

    // A tiny real executor: removing each listed world pod root settles the orphan.
    for action in &first {
        if let ConvergeAction::SweepOrphanPods { pod_dirs, .. } = action {
            for dir in pod_dirs {
                fs::remove_dir_all(dir).expect("sweep the orphan pod root");
            }
        }
    }
    let second = plan_converge(
        &engine
            .observe(&rows, &alive, &runtime)
            .expect("re-observe the store"),
    );
    assert!(
        second.is_empty(),
        "a settled store plans nothing: {second:?}"
    );
}

/// The clone disjunct of `world_data_exists`: a missing checkout with only a flat
/// `<world>.pg.pg` sibling keeps the row `broken` instead of destroying it.
#[test]
fn missing_checkout_with_a_clone_keeps_the_row_broken() {
    let tmp = tempfile::Builder::new()
        .prefix("slipfork-ledger-clone-disjunct")
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory");
    let store = tmp.path().join(".slipfork-store");
    let runtime = tmp.path().join("runtime");
    fs::create_dir_all(&runtime).expect("create the runtime root");
    let engine = BtrfsEngine::new(StoreLayout::at(store.clone()));
    let root_checkout = tmp.path().join("root");
    fs::create_dir_all(&root_checkout).expect("create the root checkout");
    fs::create_dir_all(store.join("worlds/gone--root.pg.pg"))
        .expect("plant the flat clone sibling");

    let root_row = WorldRow {
        id: 1,
        checkout_path: root_checkout,
        world_name: "root".to_owned(),
        store_root: store.clone(),
        state: WorldState::Stopped,
        supervisor_pid: None,
        pods_dir: None,
        started_at_ms: None,
        problem: None,
        updated_at_ms: 0,
    };
    let gone_row = WorldRow {
        id: 2,
        checkout_path: tmp.path().join(".forks/gone"),
        world_name: "gone--root".to_owned(),
        store_root: store.clone(),
        state: WorldState::Stopped,
        supervisor_pid: None,
        pods_dir: None,
        started_at_ms: None,
        problem: None,
        updated_at_ms: 0,
    };
    let rows = ledger_rows(vec![root_row, gone_row]);
    let observed = engine
        .observe(&rows, &BTreeSet::new(), &runtime)
        .expect("observe the store");
    assert!(
        observed.facts[&2].world_data_exists,
        "the clone sibling proves the world data exists"
    );
    assert!(
        !observed.facts[&2].checkout_exists,
        "the checkout is absent"
    );

    let plan = plan_converge(&observed);
    assert!(
        plan.iter()
            .any(|action| matches!(action, ConvergeAction::MarkBroken { world_id: 2, .. })),
        "a missing checkout keeps the row broken: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|action| matches!(action, ConvergeAction::DeleteRow { world_id: 2, .. })),
        "the clone sibling forbids a row delete: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|action| matches!(action, ConvergeAction::DestroyWorldData { world_id: 2, .. })),
        "the clone sibling forbids destroying the data: {plan:?}"
    );
}

fn ledger_rows(worlds: Vec<WorldRow>) -> slipfork_world::ledger::LedgerRows {
    slipfork_world::ledger::LedgerRows {
        worlds,
        forks: vec![ForkRow {
            world_id: 2,
            root_id: 1,
            parent_id: 1,
            name: "live".to_owned(),
            branch: String::new(),
        }],
    }
}
