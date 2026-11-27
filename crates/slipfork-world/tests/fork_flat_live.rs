//! Live (real Btrfs) proof of the flat fork family (`specs/03` §7.3–§7.4).
//!
//! Every assertion runs against real subvolumes on disk; nothing here is mocked. The suite fails
//! closed on a non-Btrfs scratch root instead of skipping, because a skipped CoW suite hides
//! broken snapshots forever.

#![cfg(feature = "live")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BtrfsEngine, ConvergeAction, Dirtiness, ForkLedger, ForkName, ForkRequest, Lease, NodeKey,
    RootRequest, StatePaths, StoreLayout, WorldEngine, WorldName, fork_checkouts, is_btrfs_path,
    is_subvolume_path, plan_converge,
};

/// One fork is a few kernel snapshot ioctls plus path bookkeeping; the family must stay far below
/// the user-visible budget even on loop-backed CI Btrfs.
const FORK_BUDGET: Duration = Duration::from_millis(500);

const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .expect("fail closed: HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>");
    PathBuf::from(home).join(".cache/slipfork/tests")
}

fn live_run_root() -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory such as ~/.cache/slipfork/tests or /var/tmp/slipfork",
        scratch.display()
    );
    fs::create_dir_all(&scratch).expect("create Btrfs scratch root");
    match is_btrfs_path(&scratch) {
        Ok(true) => {}
        Ok(false) => panic!(
            "fail closed: `{}` is not on Btrfs (BTRFS_SUPER_MAGIC 0x9123683E missing).\nnext: run on a Btrfs host or export SLIPFORK_TEST_ROOT=<btrfs dir>; {MOUNT_REMEDY}",
            scratch.display()
        ),
        Err(err) => panic!(
            "fail closed: probing `{}` failed: {err}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("flat-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create run root");
    run
}

/// The fork contract's checkpoint precondition: flush the scratch fs, then time only snapshots.
fn flush_scratch(root: &Path) {
    let dir = fs::File::open(root).expect("fail closed: open the scratch root to flush it");
    // SAFETY: `dir` is a live directory descriptor and `syncfs` only flushes that one filesystem.
    let flushed = unsafe { libc::syncfs(dir.as_raw_fd()) };
    assert_eq!(flushed, 0, "fail closed: syncfs on the scratch root failed");
}

fn subvolumes_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)
            .expect("fail closed: read the scratch tree while scanning for leftover subvolumes");
        for entry in entries {
            let entry = entry.expect("fail closed: read a scratch tree entry");
            let file_type = entry
                .file_type()
                .expect("fail closed: stat a scratch tree entry");
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            if is_subvolume_path(&path).expect("fail closed: probe a scratch tree directory") {
                found.push(path.clone());
            }
            stack.push(path);
        }
    }
    found
}

fn write_lease(path: &Path, world: &str, origin: &str) {
    let lease = Lease {
        v: 1,
        world: world.to_owned(),
        origin: origin.to_owned(),
        world_epoch: "epoch".to_owned(),
        spec: "spec".to_owned(),
        head: NodeKey::base("amd64", "sha256:ghost", "linux/amd64"),
        seeds: Vec::new(),
        services: BTreeMap::new(),
        effects: BTreeMap::new(),
    };
    lease.write_atomic(path).expect("write the lease");
}

#[test]
fn flat_fork_family_survives_depth_drop_and_convergence() {
    let run = live_run_root();
    let store = run.join(".slipfork-store");
    let engine = BtrfsEngine::new(StoreLayout::at(store.clone()));
    engine.ensure_store().expect("create .slipfork-store");

    // Root subvolume `test1` with a real project tree.
    let project = run.join("test1");
    engine
        .create_subvolume(&project)
        .expect("create the test1 subvolume");
    fs::write(project.join("Forkfile"), "# forkfile: 0.1\n").expect("seed the Forkfile");
    fs::create_dir_all(project.join("src")).expect("seed the project tree");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("seed the source");
    let root = WorldName::parse("test1").expect("valid root world");
    engine.create_world(&root).expect("create the root world");

    // 1. `fork bonjour` at the root lands flat at `test1/.forks/bonjour`.
    let bonjour = ForkName::parse("bonjour").expect("valid fork name");
    flush_scratch(&run);
    let started = Instant::now();
    let first = engine
        .fork(&project, &root, &bonjour)
        .expect("fork bonjour from the root");
    let first_ms = started.elapsed();
    assert!(
        first_ms < FORK_BUDGET,
        "forking bonjour took {first_ms:?}, budget {FORK_BUDGET:?}"
    );
    assert_eq!(first.project, project.join(".forks/bonjour"));
    assert_eq!(first.world, store.join("worlds/bonjour--test1"));
    assert_eq!(first.parent, root);
    assert_eq!(first.root, root);
    assert!(is_subvolume_path(&first.project).expect("probe bonjour's checkout"));
    assert!(is_subvolume_path(&first.world).expect("probe bonjour's world data"));

    // 2. `fork salut` FROM `test1/.forks/bonjour` (parent world `bonjour--test1`) still lands
    //    flat beside its parent, under `salut--test1`.
    let parent_world = WorldName::parse("bonjour--test1").expect("valid parent world");
    let salut = ForkName::parse("salut").expect("valid fork name");
    flush_scratch(&run);
    let started = Instant::now();
    let second = engine
        .fork(&first.project, &parent_world, &salut)
        .expect("fork salut from bonjour");
    let second_ms = started.elapsed();
    assert!(
        second_ms < FORK_BUDGET,
        "forking salut took {second_ms:?}, budget {FORK_BUDGET:?}"
    );
    assert_eq!(second.project, project.join(".forks/salut"));
    assert_eq!(second.world, store.join("worlds/salut--test1"));
    assert_eq!(second.parent, parent_world);
    assert_eq!(second.root, root);
    assert!(
        !store.join("worlds/salut--bonjour--test1").exists(),
        "world ids must never compound"
    );
    for checkout in [&first.project, &second.project] {
        assert!(
            !checkout.join(".forks").exists(),
            "`{}` must hold no `.forks`",
            checkout.display()
        );
        assert!(
            !checkout.join(".slipfork-store").exists(),
            "`{}` must hold no `.slipfork-store`",
            checkout.display()
        );
    }
    // A `.partial-*` copy stage is never advertised as a fork.
    fs::create_dir_all(project.join(".forks/.partial-crash")).expect("plant a copy stage");
    let checkouts = fork_checkouts(&project).expect("list the family checkouts");
    let names: Vec<&str> = checkouts.iter().map(|dir| dir.name.as_str()).collect();
    assert_eq!(names, ["bonjour", "salut"]);
    assert!(
        fork_checkouts(&first.project)
            .expect("list bonjour's own forks")
            .is_empty(),
        "a fork checkout advertises zero forks of its own"
    );

    // 3. Dropping the middle fork keeps its child intact.
    engine
        .drop_fork(&project, &root, &bonjour, Dirtiness::Clean, false)
        .expect("drop bonjour");
    assert!(!first.project.exists(), "bonjour's checkout is gone");
    assert!(!first.world.exists(), "bonjour's world data is gone");
    assert!(
        second.project.is_dir() && is_subvolume_path(&second.world).expect("probe salut"),
        "salut survives dropping its parent"
    );

    // 4. Clean every subvolume and prove the family left no orphan.
    engine
        .drop_fork(&project, &root, &salut, Dirtiness::Clean, true)
        .expect("drop salut");
    engine.drop_world(&root).expect("drop the root world");
    engine
        .destroy(&project, Dirtiness::Clean, true)
        .expect("destroy the root subvolume");

    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove the scratch run");
}

/// The real `ForkLedger` beside the real engine: a fork-of-fork records the immediate parent,
/// dropping the middle fork splices its child onto the family root, and the owner's convergence
/// plan stays empty (no phantom `checkout missing`) across the whole flat family.
#[test]
fn ledger_lineage_chain_splices_and_plans_clean() {
    let run = live_run_root();
    let store = run.join(".slipfork-store");
    let engine = BtrfsEngine::new(StoreLayout::at(store.clone()));
    engine.ensure_store().expect("create .slipfork-store");

    let project = run.join("test1");
    engine
        .create_subvolume(&project)
        .expect("create the test1 subvolume");
    fs::write(project.join("Forkfile"), "# forkfile: 0.1\n").expect("seed the Forkfile");
    fs::create_dir_all(project.join("src")).expect("seed the project tree");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("seed the source");
    let root = WorldName::parse("test1").expect("valid root world");
    engine.create_world(&root).expect("create the root world");

    let state = StatePaths::new(run.join("state"));
    fs::create_dir_all(&state.state_dir).expect("create the state dir");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state.state_dir.join("daemon.lock"))
        .expect("open the daemon lock");
    let ledger = ForkLedger::open_writer(state, lock).expect("open the ledger writer");

    let root_row = ledger
        .register_root(RootRequest {
            name: root.clone(),
            checkout_path: project.clone(),
            store_root: store.clone(),
            now_ms: 1,
        })
        .expect("register the root");
    let runtime = PathBuf::from(format!("/var/tmp/sf-{}", std::process::id()));

    let bonjour = ForkName::parse("bonjour").expect("valid fork name");
    let bonjour_row = ledger
        .begin_fork(ForkRequest {
            root_id: root_row.id,
            name: bonjour.clone(),
            parent_id: root_row.id,
            runtime_dir: runtime.clone(),
            branch: "bonjour".to_owned(),
            now_ms: 2,
        })
        .expect("begin bonjour");
    let first = engine
        .fork(&project, &root, &bonjour)
        .expect("fork bonjour from the root");
    assert_eq!(first.project, project.join(".forks/bonjour"));
    assert_eq!(first.world, store.join("worlds/bonjour--test1"));
    assert_eq!(first.parent, root);
    assert_eq!(bonjour_row.root_id, root_row.id);
    assert_eq!(bonjour_row.parent_id, root_row.id);

    let salut = ForkName::parse("salut").expect("valid fork name");
    let salut_row = ledger
        .begin_fork(ForkRequest {
            root_id: root_row.id,
            parent_id: bonjour_row.world_id,
            name: salut.clone(),
            runtime_dir: runtime.clone(),
            branch: "salut".to_owned(),
            now_ms: 3,
        })
        .expect("begin salut from bonjour");
    let parent_world = WorldName::parse("bonjour--test1").expect("valid parent world");
    let second = engine
        .fork(&first.project, &parent_world, &salut)
        .expect("fork salut from bonjour");
    assert_eq!(
        second.project,
        project.join(".forks/salut"),
        "flat, no nesting"
    );
    assert!(
        !first.project.join(".forks").exists() && !second.project.join(".forks").exists(),
        "no checkout holds a nested `.forks`"
    );
    assert_eq!(salut_row.parent_id, bonjour_row.world_id);
    assert_eq!(salut_row.root_id, root_row.id);

    // `family_rows` refuses a fork as the family root, and a refusal inserts nothing.
    let refused = ledger.begin_fork(ForkRequest {
        root_id: bonjour_row.world_id,
        parent_id: bonjour_row.world_id,
        name: ForkName::parse("nested").expect("valid fork name"),
        runtime_dir: runtime.clone(),
        branch: String::new(),
        now_ms: 4,
    });
    assert!(refused.is_err(), "a fork is never a family root");
    assert_eq!(ledger.list().expect("list").worlds.len(), 3);

    ledger
        .mark_stopped(bonjour_row.world_id, 5)
        .expect("stop bonjour");
    ledger
        .mark_stopped(salut_row.world_id, 6)
        .expect("stop salut");
    let rows = ledger.list().expect("list the settled family");
    let observed = engine
        .observe(&rows, &BTreeSet::new(), &runtime)
        .expect("observe");
    assert!(
        plan_converge(&observed).is_empty(),
        "a settled flat family plans nothing"
    );

    // Real-disk divergence: a crash stage, an adoptable checkout, and an orphan world + clone.
    let crash = project.join(".forks/.partial-crash");
    fs::create_dir_all(&crash).expect("plant a crash stage");
    let ghost = project.join(".forks/ghost");
    fs::create_dir_all(&ghost).expect("plant an untracked checkout");
    fs::write(ghost.join("Forkfile"), "# forkfile: 0.1\n").expect("seed ghost's Forkfile");
    let ghost_world = store.join("worlds/ghost--test1");
    fs::create_dir_all(&ghost_world).expect("create ghost's world data");
    write_lease(
        &ghost_world.join(".slipfork-lease.json"),
        "ghost--test1",
        "test1",
    );
    let orphan = store.join("worlds/orphan--test1");
    fs::create_dir_all(&orphan).expect("plant an orphan world");
    fs::create_dir_all(store.join("worlds/orphan--test1.pg.pg"))
        .expect("plant an orphan clone sibling");

    let planted = plan_converge(
        &engine
            .observe(&rows, &BTreeSet::new(), &runtime)
            .expect("observe the planted store"),
    );
    let deletes: Vec<&ConvergeAction> = planted
        .iter()
        .filter(|action| matches!(action, ConvergeAction::DeleteUntracked { .. }))
        .collect();
    assert_eq!(
        deletes.len(),
        1,
        "exactly one crash stage is deleted: {planted:?}"
    );
    assert!(matches!(
        deletes[0],
        ConvergeAction::DeleteUntracked { name, .. } if name == ".partial-crash"
    ));
    let adopts: Vec<&ConvergeAction> = planted
        .iter()
        .filter(|action| matches!(action, ConvergeAction::AdoptFork { .. }))
        .collect();
    assert_eq!(
        adopts.len(),
        1,
        "exactly one checkout is adopted: {planted:?}"
    );
    assert!(matches!(
        adopts[0],
        ConvergeAction::AdoptFork { name, parent_hint, .. }
            if name == "ghost" && parent_hint.as_deref() == Some("test1")
    ));
    let orphans: Vec<&ConvergeAction> = planted
        .iter()
        .filter(|action| matches!(action, ConvergeAction::DestroyOrphan { .. }))
        .collect();
    assert_eq!(
        orphans.len(),
        1,
        "the clone sibling dedupes into one orphan action: {planted:?}"
    );
    assert!(matches!(
        orphans[0],
        ConvergeAction::DestroyOrphan { name, .. } if name == "orphan--test1"
    ));

    // Remove the planted checkout/stage; `drop_world` sweeps `orphan--test1*` later.
    fs::remove_dir_all(&crash).expect("remove the crash stage");
    fs::remove_dir_all(&ghost).expect("remove the ghost checkout");
    fs::remove_dir_all(&ghost_world).expect("remove the ghost world data");
    fs::remove_dir_all(store.join("worlds/orphan--test1.pg.pg")).expect("remove the orphan clone");
    fs::remove_dir_all(&orphan).expect("remove the orphan world");

    // Drop the middle fork: its child survives and is spliced onto the family root.
    ledger
        .mark_deleting(bonjour_row.world_id, 7)
        .expect("mark bonjour deleting");
    engine
        .drop_fork(&project, &root, &bonjour, Dirtiness::Clean, false)
        .expect("drop bonjour");
    ledger
        .delete_world_row(bonjour_row.world_id)
        .expect("delete the bonjour row");
    assert!(
        !first.project.exists() && !first.world.exists(),
        "bonjour is gone"
    );
    assert!(
        second.project.is_dir() && second.world.exists(),
        "salut survives dropping its parent"
    );
    let rows = ledger.list().expect("list after the splice");
    let spliced = rows
        .forks
        .iter()
        .find(|fork| fork.world_id == salut_row.world_id)
        .expect("salut's row survives");
    assert_eq!(
        spliced.parent_id, root_row.id,
        "delete_world_row splices the child onto the root"
    );
    let observed = engine
        .observe(&rows, &BTreeSet::new(), &runtime)
        .expect("observe after the drop");
    assert!(
        plan_converge(&observed).is_empty(),
        "no phantom `checkout missing` after the splice"
    );

    ledger
        .mark_deleting(salut_row.world_id, 8)
        .expect("mark salut deleting");
    engine
        .drop_fork(&project, &root, &salut, Dirtiness::Clean, false)
        .expect("drop salut");
    ledger
        .delete_world_row(salut_row.world_id)
        .expect("delete the salut row");
    engine.drop_world(&root).expect("drop the root world");
    engine
        .destroy(&project, Dirtiness::Clean, true)
        .expect("destroy the project subvolume");
    drop(ledger);
    let _ = fs::remove_dir_all(&runtime);

    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove the scratch run");
}
