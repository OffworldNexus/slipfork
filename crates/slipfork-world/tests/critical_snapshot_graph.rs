//! Protected real-Btrfs journey for the one snapshot graph (`--features live`).
//!
//! Every assertion runs against real Btrfs subvolumes on disk. A non-Btrfs scratch root FAILS
//! CLOSED instead of skipping, because a skipped CoW suite hides broken snapshots forever.
use slipfork_world::{
    BtrfsEngine, BuildGraph, Cache, Dirtiness, GcOutcome, LEASE_FILE, Lease, LeaseEntry, Node,
    NodeKey, NodeKind, Parallelism, PublishOutcome, SnapshotError, SnapshotResult, SnapshotSlot,
    SnapshotStore, StoreLayout, WorldEngine, is_btrfs_path, is_subvolume_path,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";
/// Success-path reconcile with a silent progress sink; failure paths stay explicit.
macro_rules! run {
    ($store:expr, $graph:expr, $cache:expr, $workers:expr, $now:expr, $materialize:expr) => {
        $store.reconcile(
            $graph,
            $cache,
            Parallelism::new($workers),
            $now,
            &mut |_| {},
            $materialize,
        )
    };
}
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
        Err(error) => panic!(
            "fail closed: probing `{}` failed: {error}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("snapshot-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create run root");
    run
}
fn node(key: NodeKey, kind: NodeKind, parents: Vec<NodeKey>, canon: &str, line: u32) -> Node {
    Node {
        key,
        kind,
        parents,
        canon: canon.to_owned(),
        line,
    }
}
fn record_json(store: &SnapshotStore<'_>, key: &NodeKey) -> serde_json::Value {
    let text = fs::read_to_string(store.record_path(key)).expect("fail closed: read a node record");
    serde_json::from_str(&text).expect("fail closed: parse a node record")
}
fn sealed(engine: &BtrfsEngine, path: &Path) -> bool {
    engine
        .is_sealed(path)
        .expect("fail closed: probe BTRFS_SUBVOL_RDONLY")
}
fn cleanup(engine: &BtrfsEngine, root: &Path) {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if is_subvolume_path(&dir).unwrap_or(false) {
            found.push(dir.clone());
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    stack.push(entry.path());
                }
            }
        }
    }
    found.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in found {
        let _ = engine.destroy(&path, Dirtiness::Clean, true);
    }
    let _ = fs::remove_dir_all(root);
}
#[test]
fn snapshot_graph_lifecycle() {
    let run = live_run_root();
    let layout = StoreLayout::at(run.join(".slipfork-store"));
    let engine = BtrfsEngine::new(layout.clone());
    engine.ensure_store().expect("create .slipfork-store");
    let store = SnapshotStore::new(&layout, &engine);
    fs::create_dir_all(store.snapshots_root()).expect("create snapshots root");
    let base = NodeKey::base("amd64", "sha256:1111", "linux/amd64");
    let step1 = NodeKey::step("amd64", &[base], "RUN one", "cfg-1", &[]);
    let step2 = NodeKey::step("amd64", &[step1], "RUN two", "cfg-1", &[]);
    let warm =
        |node: &Node, slot: &Path, p: &mut dyn FnMut(&str)| -> SnapshotResult<serde_json::Value> {
            p(node.canon.as_str());
            if node.kind.is_materialized() {
                fs::write(slot.join("content"), node.canon.as_bytes())
                    .map_err(|source| SnapshotError::io("write", slot, source))?;
            }
            Ok(serde_json::json!({ "canon": node.canon }))
        };
    let graph = BuildGraph::new(
        step2,
        vec![
            node(base, NodeKind::Base, vec![], "FROM base", 1),
            node(step1, NodeKind::Step, vec![base], "RUN one", 2),
            node(step2, NodeKind::Step, vec![step1], "RUN two", 3),
        ],
    )
    .expect("valid graph");
    let mut lines = Vec::new();
    let first = store
        .reconcile(
            &graph,
            Cache::Warm,
            Parallelism::new(2),
            1_000,
            &mut |line: &str| lines.push(line.to_owned()),
            warm,
        )
        .expect("first reconcile");
    assert_eq!((first.built, first.reused), (3, 0));
    assert_eq!(lines.len(), 3, "one progress line per built node");
    assert!(
        lines.iter().all(|line| {
            let (hex, canon) = line.split_once(' ').expect("12-hex key then canon");
            hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()) && !canon.is_empty()
        }),
        "every line is `<12-hex> <canon>`"
    );
    for expected in [
        format!("{} FROM base", &base.as_hex()[..12]),
        format!("{} RUN two", &step2.as_hex()[..12]),
    ] {
        assert!(lines.contains(&expected), "{expected} reached the sink");
    }
    assert!(sealed(&engine, &store.node_dir(&base)));
    assert!(sealed(&engine, &store.node_dir(&step2)));
    assert_eq!(
        fs::read_to_string(store.node_dir(&step2).join("content")).expect("node content"),
        "RUN two"
    );
    assert_eq!(record_json(&store, &step2)["kind"], "step");
    let second = run!(&store, &graph, Cache::Warm, 1, 2_000, warm).expect("warm reconcile");
    assert_eq!((second.built, second.reused), (0, 3));
    assert_eq!(record_json(&store, &step2)["last_used_ms"], 2_000);
    let step2b = NodeKey::step("amd64", &[step1], "RUN two v2", "cfg-1", &[]);
    let edited = BuildGraph::new(
        step2b,
        vec![
            node(base, NodeKind::Base, vec![], "FROM base", 1),
            node(step1, NodeKind::Step, vec![base], "RUN one", 2),
            node(step2b, NodeKind::Step, vec![step1], "RUN two v2", 3),
        ],
    )
    .expect("edited graph");
    let suffix = run!(&store, &edited, Cache::Warm, 1, 3_000, warm).expect("suffix reconcile");
    assert_eq!((suffix.built, suffix.reused), (1, 2));
    assert!(sealed(&engine, &store.node_dir(&step2)));
    assert!(sealed(&engine, &store.node_dir(&step2b)));
    let base2 = NodeKey::base("amd64", "sha256:2222", "linux/amd64");
    let good = NodeKey::step("amd64", &[base2], "RUN good", "cfg-2", &[]);
    let bad = NodeKey::step("amd64", &[base2], "RUN boom", "cfg-2", &[]);
    let world = NodeKey::world(&[good, bad], "failure-spec");
    let failing_graph = BuildGraph::new(
        world,
        vec![
            node(base2, NodeKind::Base, vec![], "FROM base2", 1),
            node(good, NodeKind::Step, vec![base2], "RUN good", 2),
            node(bad, NodeKind::Step, vec![base2], "RUN boom", 3),
            node(world, NodeKind::World, vec![good, bad], "world", 4),
        ],
    )
    .expect("failing graph");
    let failing =
        |node: &Node, slot: &Path, p: &mut dyn FnMut(&str)| -> SnapshotResult<serde_json::Value> {
            p(node.canon.as_str());
            if node.key == bad {
                return Err(SnapshotError::message("the test materializer refused"));
            }
            if node.kind.is_materialized() {
                fs::write(slot.join("content"), node.canon.as_bytes())
                    .map_err(|source| SnapshotError::io("write", slot, source))?;
            }
            Ok(serde_json::Value::Null)
        };
    let mut failed_lines = Vec::new();
    let failed = store
        .reconcile(
            &failing_graph,
            Cache::Warm,
            Parallelism::new(2),
            4_000,
            &mut |line: &str| failed_lines.push(line.to_owned()),
            failing,
        )
        .expect_err("the failing node must fail the reconcile");
    assert_eq!((failed.node, failed.line, failed.built), (bad, 3, 1));
    let good_line = format!("{} RUN good", &good.as_hex()[..12]);
    assert!(failed_lines.contains(&good_line), "good forwarded progress");
    assert!(sealed(&engine, &store.node_dir(&base2)));
    assert!(sealed(&engine, &store.node_dir(&good)));
    assert!(!store.node_dir(&bad).exists());
    assert!(
        !store.node_dir(&world).exists(),
        "the head was never reached"
    );
    assert!(!store.record_path(&world).exists(), "no head record");
    let leftovers = fs::read_dir(store.snapshots_root())
        .expect("read snapshots root")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
        .count();
    assert_eq!(leftovers, 0, "every own slot is destroyed on failure");
    let cold_key = NodeKey::step("amd64", &[step1], "RUN cold", "cfg-1", &[]);
    let cold_graph = BuildGraph::new(
        cold_key,
        vec![
            node(base, NodeKind::Base, vec![], "FROM base", 1),
            node(step1, NodeKind::Step, vec![base], "RUN one", 2),
            node(cold_key, NodeKind::Step, vec![step1], "RUN cold", 3),
        ],
    )
    .expect("cold graph");
    run!(&store, &cold_graph, Cache::Warm, 1, 5_000, warm).expect("seed the cold key");
    let legacy = layout.pods_root().join("example").join("legacy");
    engine
        .clone_subvolume(&store.node_dir(&cold_key), &legacy)
        .expect("clone a pod tree from the sealed twin");
    let cold = |_node: &Node,
                slot: &Path,
                _p: &mut dyn FnMut(&str)|
     -> SnapshotResult<serde_json::Value> {
        if _node.kind.is_materialized() {
            fs::write(slot.join("content"), b"cold v2")
                .map_err(|source| SnapshotError::io("write", slot, source))?;
        }
        Ok(serde_json::json!({ "rebuilt": true }))
    };
    run!(&store, &cold_graph, Cache::Cold, 1, 6_000, cold).expect("cold reconcile");
    assert_eq!(
        fs::read_to_string(legacy.join("content")).expect("legacy pod content"),
        "RUN cold",
        "a cloned pod keeps the old CoW content"
    );
    assert_eq!(
        fs::read_to_string(store.node_dir(&cold_key).join("content")).expect("cold content"),
        "cold v2"
    );
    let base3 = NodeKey::base("amd64", "sha256:3333", "linux/amd64");
    let step3 = NodeKey::step("amd64", &[base3], "RUN three", "cfg-3", &[]);
    let image3 = NodeKey::image("amd64", &step3, "image three", "cfg-3");
    let seed3 = NodeKey::seed("amd64", &image3, &["initdb".to_owned()], "v1");
    let closure_graph = BuildGraph::new(
        seed3,
        vec![
            node(base3, NodeKind::Base, vec![], "FROM base3", 1),
            node(step3, NodeKind::Step, vec![base3], "RUN three", 2),
            node(image3, NodeKind::Image, vec![step3], "image three", 3),
            node(seed3, NodeKind::Seed, vec![image3], "seed", 4),
        ],
    )
    .expect("closure graph");
    let closure =
        run!(&store, &closure_graph, Cache::Warm, 2, 7_000, warm).expect("closure reconcile");
    assert_eq!(
        closure.built, 3,
        "three sealed nodes; the image is record-only"
    );
    assert_eq!(record_json(&store, &image3)["kind"], "image");
    assert!(!store.node_dir(&image3).exists());
    assert_eq!(
        fs::read_to_string(store.node_dir(&step3).join("content")).expect("step3 content"),
        "RUN three",
        "the record-only image never wrote through its sealed parent"
    );
    let unmarked = NodeKey::base("amd64", "sha256:unmarked", "linux/amd64");
    let unmarked_graph = BuildGraph::new(
        unmarked,
        vec![node(unmarked, NodeKind::Base, vec![], "FROM unmarked", 1)],
    )
    .expect("unmarked graph");
    run!(&store, &unmarked_graph, Cache::Warm, 1, 8_000, warm).expect("build the unmarked node");
    let service_key = NodeKey::service("amd64", &image3, "api up", "spec-digest");
    let mut lease = Lease {
        v: 1,
        world: "demo".to_owned(),
        origin: "demo".to_owned(),
        world_epoch: "k3v9".to_owned(),
        spec: "spec-digest".to_owned(),
        head: NodeKey::base("amd64", "placeholder", "p"),
        seeds: vec![seed3],
        services: BTreeMap::from([(
            "api".to_owned(),
            LeaseEntry {
                key: service_key,
                image: image3,
            },
        )]),
        effects: BTreeMap::new(),
    };
    lease.head = lease.derived_head();
    let world_dir = layout.worlds_root().join("demo");
    fs::create_dir_all(&world_dir).expect("create world dir");
    let lease_path = world_dir.join(LEASE_FILE);
    lease.write_atomic(&lease_path).expect("write lease");
    let sweep = store.gc(9_000, 0, &[]).expect("zero-grace gc");
    match &sweep {
        GcOutcome::Swept(report) => {
            assert!(report.pruned.contains(&unmarked));
            for leased in [base3, step3, image3, seed3] {
                assert!(!report.pruned.contains(&leased), "{leased} is leased");
            }
        }
        GcOutcome::Skipped { reason } => panic!("zero-grace gc skipped: {reason}"),
    }
    assert!(sealed(&engine, &store.node_dir(&step3)));
    let stale_base = NodeKey::base("amd64", "sha256:5555", "linux/amd64");
    let stale_step = NodeKey::step("amd64", &[stale_base], "RUN root", "cfg-5", &[]);
    let seed_g = BuildGraph::new(
        stale_step,
        vec![
            node(stale_base, NodeKind::Base, vec![], "FROM s", 1),
            node(stale_step, NodeKind::Step, vec![stale_base], "RUN root", 2),
        ],
    )
    .expect("stale seed graph");
    let seeded = run!(&store, &seed_g, Cache::Warm, 1, 10_000, warm).expect("seed stale prefix");
    assert_eq!(seeded.built, 2);
    let stale_unsealed = NodeKey::step("amd64", &[stale_step], "RUN su", "cfg-1", &[]);
    let stale_plain = NodeKey::step("amd64", &[stale_step], "RUN sp", "cfg-1", &[]);
    engine
        .create_subvolume(&store.node_dir(&stale_unsealed))
        .expect("pre-create an unsealed subvolume");
    fs::create_dir_all(store.node_dir(&stale_plain)).expect("pre-create a plain dir");
    let stale_world = NodeKey::world(&[stale_unsealed, stale_plain], "stale-spec");
    let stale_graph = BuildGraph::new(
        stale_world,
        vec![
            node(stale_base, NodeKind::Base, vec![], "FROM s", 1),
            node(stale_step, NodeKind::Step, vec![stale_base], "RUN root", 2),
            node(
                stale_unsealed,
                NodeKind::Step,
                vec![stale_step],
                "RUN su",
                3,
            ),
            node(stale_plain, NodeKind::Step, vec![stale_step], "RUN sp", 4),
            node(
                stale_world,
                NodeKind::World,
                vec![stale_unsealed, stale_plain],
                "world",
                5,
            ),
        ],
    )
    .expect("stale graph");
    let rebuilt =
        run!(&store, &stale_graph, Cache::Warm, 2, 11_000, warm).expect("rebuild stale entries");
    assert_eq!(rebuilt.built, 2);
    assert!(
        !store.node_dir(&stale_world).exists(),
        "world head has no subvolume"
    );
    for key in [stale_unsealed, stale_plain] {
        assert!(sealed(&engine, &store.node_dir(&key)));
        assert_eq!(record_json(&store, &key)["kind"], "step");
    }
    fs::write(store.record_path(&stale_unsealed), b"garbage").expect("corrupt a record");
    let repaired = run!(&store, &stale_graph, Cache::Warm, 1, 12_000, warm)
        .expect("repair the corrupt record");
    assert_eq!(repaired.built, 1);
    assert_eq!(record_json(&store, &stale_unsealed)["canon"], "RUN su");
    let adopted_key = NodeKey::step("amd64", &[step1], "RUN adopted", "cfg-1", &[]);
    let mut slot =
        SnapshotSlot::create(&engine, store.snapshots_root(), adopted_key, 999_999, None)
            .expect("create a slot");
    slot.seal().expect("seal the slot");
    let winner = store.node_dir(&adopted_key);
    engine.create_subvolume(&winner).expect("create the winner");
    fs::write(winner.join("marker"), b"winner").expect("mark the winner");
    assert_eq!(
        slot.publish().expect("publish loses the race"),
        PublishOutcome::Adopted
    );
    assert!(!slot.path().exists(), "the losing slot is destroyed");
    assert_eq!(
        fs::read_to_string(winner.join("marker")).expect("winner marker"),
        "winner"
    );
    let sf_base = NodeKey::base("amd64", "sha256:4444", "linux/amd64");
    let sf_step = NodeKey::step("amd64", &[sf_base], "RUN singleflight", "cfg-4", &[]);
    let sf_graph = BuildGraph::new(
        sf_step,
        vec![
            node(sf_base, NodeKind::Base, vec![], "FROM sf", 1),
            node(
                sf_step,
                NodeKind::Step,
                vec![sf_base],
                "RUN singleflight",
                2,
            ),
        ],
    )
    .expect("singleflight graph");
    let hits = Arc::new(Mutex::new(Vec::<NodeKey>::new()));
    let recording =
        |node: &Node, slot: &Path, _p: &mut dyn FnMut(&str)| -> SnapshotResult<serde_json::Value> {
            hits.lock().expect("hits").push(node.key);
            if node.kind.is_materialized() {
                fs::write(slot.join("content"), node.canon.as_bytes())
                    .map_err(|source| SnapshotError::io("write", slot, source))?;
            }
            Ok(serde_json::Value::Null)
        };
    std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            store.reconcile(
                &sf_graph,
                Cache::Warm,
                Parallelism::new(2),
                12_000,
                &mut |_| {},
                recording,
            )
        });
        let two = scope.spawn(|| {
            store.reconcile(
                &sf_graph,
                Cache::Warm,
                Parallelism::new(2),
                12_001,
                &mut |_| {},
                recording,
            )
        });
        assert!(one.join().expect("first thread").is_ok());
        assert!(two.join().expect("second thread").is_ok());
    });
    let mut counts: BTreeMap<NodeKey, usize> = BTreeMap::new();
    for key in hits.lock().expect("hits").iter().copied() {
        *counts.entry(key).or_default() += 1;
    }
    assert_eq!(counts.get(&sf_base), Some(&1), "base built once");
    assert_eq!(counts.get(&sf_step), Some(&1), "step built once");
    fs::write(&lease_path, b"not a lease").expect("corrupt the lease");
    assert!(matches!(
        store.gc(13_000, 0, &[]),
        Ok(GcOutcome::Skipped { .. })
    ));
    lease.write_atomic(&lease_path).expect("restore lease");
    let lock_path = store.snapshots_root().join(".lock");
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open the snapshots lock");
    // SAFETY: `flock(2)` only consumes the live descriptor.
    let rc = unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_SH) };
    assert_eq!(rc, 0, "hold LOCK_SH");
    assert!(matches!(
        store.gc(14_000, 0, &[]),
        Ok(GcOutcome::Skipped { .. })
    ));
    drop(held);
    let dead_slot = store.snapshots_root().join(".tmp-dead-0-0");
    engine
        .create_subvolume(&dead_slot)
        .expect("create a dead slot");
    assert!(matches!(store.gc(15_000, 0, &[]), Ok(GcOutcome::Swept(_))));
    assert!(!dead_slot.exists());
    cleanup(&engine, &run);
}
