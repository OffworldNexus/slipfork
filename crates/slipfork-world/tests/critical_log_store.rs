//! Lab: the single store-root and run-catalog owner on real disk (`/var/tmp`), zero mocks.
//!
//! Covers both store-root branches (a `/`-rooted or read-only mount must resolve under the
//! injected `data_home`, never `/.slipfork-store`), the begin -> record -> records roundtrip on
//! real capture files, tolerant catalog reads, and retention that keeps a failed run forever.

use std::fs;
use std::path::PathBuf;

use slipfork_world::run_log::{LAST_FAILED_FILE, LAST_FILE, RunCapture, RunRecord};
use slipfork_world::{LogStore, Stream, WorldName};

fn temp(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory")
}

fn world() -> WorldName {
    WorldName::parse("demo").expect("valid world name")
}

fn store(lab: &tempfile::TempDir) -> LogStore {
    LogStore::at(lab.path().join("store"), world())
}

fn seed(store: &LogStore, state: &str, started_ms: u64) -> String {
    let run_id = format!("{started_ms:06x}");
    fs::create_dir_all(store.run_dir()).expect("create the run directory");
    fs::write(store.capture(&run_id), b"captured output\n").expect("write a real capture");
    fs::write(store.capture(&run_id).with_extension("idx"), [0_u8; 16])
        .expect("write a real index companion");
    store
        .record(&RunRecord {
            run_id: run_id.clone(),
            kind: "setup".to_owned(),
            target: "backend".to_owned(),
            command: "uv sync".to_owned(),
            phase: String::new(),
            instruction: String::new(),
            source: String::new(),
            exit_code: None,
            reason: None,
            state: state.to_owned(),
            started_ms,
            finished_ms: Some(started_ms + 5),
            duration_ms: 5,
            project: "/var/tmp/apps/demo".to_owned(),
            world: "demo".to_owned(),
        })
        .expect("append a catalog line");
    run_id
}

#[test]
fn root_for_never_resolves_to_root_and_honours_writability() {
    let lab = temp("log-store-root");
    let data_home = lab.path().join("data");

    let root_project = lab.path().join("apps/demo");
    fs::create_dir_all(&root_project).expect("create the project");
    let root_mounts = vec![PathBuf::from("/")];
    let rooted = LogStore::root_for(&root_mounts, &root_project, &data_home, true);
    assert_eq!(rooted, data_home.join("slipfork").join("store"));
    assert_ne!(rooted, PathBuf::from("/.slipfork-store"));

    let btrfs = lab.path().join("btrfs");
    let btrfs_project = btrfs.join("apps/demo");
    fs::create_dir_all(&btrfs_project).expect("create the nested project");
    let mounts = vec![PathBuf::from("/"), btrfs.clone()];
    assert_eq!(
        LogStore::root_for(&mounts, &btrfs_project, &data_home, true),
        btrfs.join(".slipfork-store")
    );
    assert_eq!(
        LogStore::root_for(&mounts, &btrfs_project, &data_home, false),
        data_home.join("slipfork").join("store"),
        "a read-only mount resolves under data_home"
    );
    assert_eq!(
        LogStore::root_for(&[], &btrfs_project, &data_home, true),
        data_home.join("slipfork").join("store"),
        "an empty mount table resolves under data_home"
    );
}

#[test]
fn begin_record_and_records_roundtrip_through_one_log_store() {
    let lab = temp("log-store-roundtrip");
    let store = store(&lab);

    let mut capture = RunCapture::begin(
        &store,
        &lab.path().join("apps/demo"),
        "setup",
        "backend",
        "uv sync",
    )
    .expect("begin a run");
    let run_id = capture.run_id().to_owned();
    capture
        .append_stream(Stream::Stdout, "resolved 12 packages")
        .expect("append stdout");
    capture.finish("ready", 41).expect("finish the run");

    assert!(store.capture(&run_id).is_file(), "capture file exists");
    assert!(
        store.capture(&run_id).with_extension("idx").is_file(),
        "index companion exists"
    );
    assert!(store.index().is_file(), "catalog exists");

    let records = store.records().expect("read the catalog").records;
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.run_id, run_id);
    assert_eq!(record.kind, "setup");
    assert_eq!(record.target, "backend");
    assert_eq!(record.command, "uv sync");
    assert_eq!(record.state, "ready");
    assert_eq!(record.duration_ms, 41);
    assert!(
        record
            .finished_ms
            .is_some_and(|finished| finished >= record.started_ms)
    );
    assert_eq!(record.world, "demo");
    assert!(record.project.ends_with("apps/demo"));

    store
        .record(&RunRecord {
            run_id: "0a1b2c".to_owned(),
            kind: "image".to_owned(),
            target: "base".to_owned(),
            command: "base".to_owned(),
            phase: String::new(),
            instruction: String::new(),
            source: String::new(),
            exit_code: None,
            reason: None,
            state: "built".to_owned(),
            started_ms: record.started_ms + 100,
            finished_ms: Some(record.started_ms + 150),
            duration_ms: 50,
            project: "/var/tmp/apps/demo".to_owned(),
            world: "demo".to_owned(),
        })
        .expect("append a second catalog line");
    assert_eq!(store.records().expect("read back").records.len(), 2);
}

#[test]
fn records_skip_old_schema_and_distorted_lines_instead_of_bricking() {
    let lab = temp("log-store-tolerant");
    let store = store(&lab);
    let run_id = seed(&store, "ready", 1_000);

    let mut text = fs::read_to_string(store.index()).expect("read the catalog");
    text.push_str(
        "{\"run_id\":\"deadbe\",\"ts_ms\":1,\"kind\":\"setup\",\"name\":\"backend\",\
         \"command\":\"uv sync\",\"state\":\"ready\",\"duration_ms\":1}\n",
    );
    text.push_str("{\"run_id\":\"trunc\"\n");
    fs::write(store.index(), text).expect("append the damaged lines");

    let catalog = store.records().expect("a corrupt catalog still reads");
    assert_eq!(catalog.records.len(), 1, "{catalog:?}");
    assert_eq!(catalog.records[0].run_id, run_id);
    assert_eq!(catalog.skipped, 2, "old-schema and truncated lines counted");
}

#[test]
fn retain_prunes_old_successes_but_never_deletes_a_failed_run() {
    let lab = temp("log-store-retain");
    let store = store(&lab);

    let failed = seed(&store, "failed", 10);
    let old_success = seed(&store, "ready", 20);
    let mid_success = seed(&store, "ready", 30);
    let new_success = seed(&store, "ready", 40);

    let pruned = store.retain(2).expect("retain the newest two successes");
    assert_eq!(pruned, 1, "only the oldest success is pruned");

    assert!(
        !store.capture(&old_success).exists(),
        "the pruned `.log` is gone"
    );
    assert!(
        !store.capture(&old_success).with_extension("idx").exists(),
        "the pruned `.idx` is gone"
    );
    assert!(
        store.capture(&failed).is_file(),
        "a failed capture survives every prune"
    );
    assert!(store.capture(&mid_success).is_file());
    assert!(store.capture(&new_success).is_file());

    let records = store.records().expect("read the pruned catalog").records;
    let ids: Vec<&str> = records
        .iter()
        .map(|record| record.run_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![failed.as_str(), mid_success.as_str(), new_success.as_str()]
    );

    let dir = store.run_dir();
    assert_eq!(
        fs::read_to_string(dir.join(LAST_FILE))
            .expect("last pointer")
            .trim(),
        new_success
    );
    assert_eq!(
        fs::read_to_string(dir.join(LAST_FAILED_FILE))
            .expect("last-failed pointer")
            .trim(),
        failed
    );

    store.retain(0).expect("retain nothing but failures");
    let records = store.records().expect("read the pruned catalog").records;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].run_id, failed);
    assert!(store.capture(&failed).is_file());
    assert_eq!(
        fs::read_to_string(dir.join(LAST_FAILED_FILE))
            .expect("last-failed pointer")
            .trim(),
        failed
    );
}

/// A crash leaves `running` with no terminal line; retention must never sweep that evidence.
#[test]
fn retain_never_sweeps_a_crashed_running_run_or_a_failed_run() {
    let lab = temp("log-store-retain-crash");
    let store = store(&lab);

    let successes: Vec<String> = (0..25)
        .map(|step| seed(&store, "ready", 1_000 + step))
        .collect();
    let crashed = seed(&store, "running", 2_000);
    let failed = seed(&store, "failed", 2_001);

    assert_eq!(store.retain(2).expect("retain two successes"), 23);

    for id in &successes[..23] {
        assert!(!store.capture(id).exists(), "old success pruned: {id}");
        assert!(
            !store.capture(id).with_extension("idx").exists(),
            "old success index pruned: {id}"
        );
    }
    for id in [&successes[23], &successes[24]] {
        assert!(store.capture(id).is_file(), "newest success kept: {id}");
    }
    for (label, id) in [("crashed", &crashed), ("failed", &failed)] {
        assert!(
            store.capture(id).is_file(),
            "{label} capture survives retention"
        );
        assert!(
            store.capture(id).with_extension("idx").is_file(),
            "{label} index companion survives retention"
        );
    }

    let mut states: Vec<(String, String)> = store
        .records()
        .expect("read the pruned catalog")
        .records
        .into_iter()
        .map(|record| (record.run_id, record.state))
        .collect();
    states.sort();
    assert_eq!(
        states.len(),
        4,
        "two successes + crashed + failed: {states:?}"
    );
    assert_eq!(
        states.iter().filter(|(_, state)| state == "ready").count(),
        2
    );
    assert!(
        states
            .iter()
            .any(|(id, state)| id == &crashed && state == "running")
    );
    assert!(
        states
            .iter()
            .any(|(id, state)| id == &failed && state == "failed")
    );
}

#[test]
fn catalog_keeps_the_newest_line_for_a_duplicate_run_id() {
    let lab = temp("log-store-duplicate");
    let store = store(&lab);
    let mut record = RunRecord {
        run_id: "abc123".to_owned(),
        kind: "setup".to_owned(),
        target: "backend".to_owned(),
        command: "uv sync".to_owned(),
        phase: String::new(),
        instruction: String::new(),
        source: String::new(),
        exit_code: None,
        reason: None,
        state: "ready".to_owned(),
        started_ms: 10,
        finished_ms: Some(20),
        duration_ms: 10,
        project: "/var/tmp/apps/demo".to_owned(),
        world: "demo".to_owned(),
    };
    store.record(&record).expect("append the first line");
    record.state = "failed".to_owned();
    record.started_ms = 30;
    store.record(&record).expect("append the newest line");

    let catalog = store.records().expect("read the catalog");
    assert_eq!(catalog.records.len(), 1, "one entry per run id");
    assert_eq!(catalog.records[0].state, "failed", "newest line wins");
    assert_eq!(catalog.skipped, 0);
}

#[test]
fn retain_zero_with_only_successes_removes_captures_and_pointers() {
    let lab = temp("log-store-retain-zero");
    let store = store(&lab);
    let first = seed(&store, "ready", 10);
    let second = seed(&store, "ready", 20);

    assert_eq!(store.retain(0).expect("retain no successes"), 2);
    for id in [&first, &second] {
        assert!(!store.capture(id).exists(), "capture removed: {id}");
        assert!(
            !store.capture(id).with_extension("idx").exists(),
            "index removed: {id}"
        );
    }
    let dir = store.run_dir();
    assert!(!dir.join(LAST_FILE).exists(), "no `last` pointer remains");
    assert!(
        !dir.join(LAST_FAILED_FILE).exists(),
        "no `last-failed` pointer remains"
    );
    assert!(store.records().expect("empty catalog").records.is_empty());
}
