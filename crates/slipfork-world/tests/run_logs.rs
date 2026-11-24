//! Lab: the per-step run catalog on a real disk (`/var/tmp`), zero mocks.
//!
//! One executed step must mint one unique 6-char id, leave a framed `.log` plus its `.idx`, and
//! append exactly one `index.jsonl` line; a discarded no-op must leave nothing behind.

use slipfork_world::LogStore;
use slipfork_world::WorldName;
use slipfork_world::pod_log::read_range;
use slipfork_world::run_log::{LAST_FAILED_FILE, LAST_FILE, RunCapture};

fn temp(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory")
}

fn store(root: &tempfile::TempDir) -> LogStore {
    LogStore::at(
        root.path().to_path_buf(),
        WorldName::parse("demo").expect("valid world name"),
    )
}

#[test]
fn an_executed_step_mints_one_unique_id_capture_pair_and_catalog_line() {
    let root = temp("run-log-executed");
    let store = store(&root);
    let project = root.path().join("apps/demo");

    let mut first = RunCapture::begin(&store, &project, "setup", "backend", "uv sync")
        .expect("begin a real run capture");
    let first_id = first.run_id().to_owned();
    assert_eq!(first_id.len(), 6, "{first_id}");
    assert!(
        first_id
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "lowercase hex: {first_id}"
    );
    first.append("Resolved 12 packages").expect("append stdout");
    first
        .append("error: Failed to resolve `numpy==9`")
        .expect("append stderr");
    first.finish("ready", 41).expect("finish the run");

    let mut second = RunCapture::begin(&store, &project, "hook", "api:pre-deploy", "uv run seed")
        .expect("begin a second capture");
    let second_id = second.run_id().to_owned();
    assert_ne!(first_id, second_id, "ids are unique per executed step");
    second.append("seeded 3 rows").expect("append");
    second.finish("failed", 1_234).expect("finish the run");

    let dir = store.run_dir();
    for id in [&first_id, &second_id] {
        assert!(dir.join(format!("{id}.log")).is_file(), "missing {id}.log");
        assert!(dir.join(format!("{id}.idx")).is_file(), "missing {id}.idx");
    }

    let records = store.records().expect("read the catalog").records;
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].run_id, first_id);
    assert_eq!(records[0].kind, "setup");
    assert_eq!(records[0].target, "backend");
    assert_eq!(records[0].command, "uv sync");
    assert_eq!(records[0].state, "ready");
    assert_eq!(records[0].duration_ms, 41);
    assert!(records[0].started_ms > 0);
    assert!(records[0].finished_ms.is_some());
    assert_eq!(records[0].world, "demo");
    assert!(records[0].project.ends_with("apps/demo"));
    assert_eq!(records[1].state, "failed");
    assert_eq!(records[1].duration_ms, 1_234);

    assert_eq!(
        std::fs::read_to_string(dir.join(LAST_FILE))
            .expect("last pointer")
            .trim(),
        second_id
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(LAST_FAILED_FILE))
            .expect("last-failed pointer")
            .trim(),
        second_id
    );
    assert!(
        std::fs::read_to_string(store.index())
            .expect("catalog text")
            .lines()
            .count()
            == 2
    );

    let resolved =
        slipfork_world::run_log::resolve_run(&store.world_logs(), &format!("#{first_id}"))
            .expect("resolve #id");
    assert_eq!(resolved, first_id);
    assert_eq!(
        slipfork_world::run_log::resolve_run(&store.world_logs(), "last").expect("resolve last"),
        second_id
    );
    assert_eq!(
        slipfork_world::run_log::resolve_run(&store.world_logs(), "failed")
            .expect("resolve failed"),
        second_id
    );

    let mut reader = read_range(&dir, &first_id, None, None).expect("open the run capture");
    let mut lines = Vec::new();
    let _ = reader.read_lines(&mut |line| lines.push(line.text.clone()));
    reader.flush(&mut |line| lines.push(line.text.clone()));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Failed to resolve `numpy==9`")),
        "the real stderr replays: {lines:?}"
    );
    assert!(
        slipfork_world::run_log::resolve_run(&store.world_logs(), "deadbe")
            .unwrap_err()
            .to_string()
            .contains("no recorded run")
    );
}

#[test]
fn a_cached_no_op_mints_no_id_and_leaves_no_run_files() {
    let root = temp("run-log-cached");
    let store = store(&root);
    let project = root.path().join("apps/demo");

    let capture = RunCapture::begin(&store, &project, "image", "base", "base")
        .expect("begin a real run capture");
    let id = capture.run_id().to_owned();
    capture.discard();

    let dir = store.run_dir();
    assert!(
        !dir.join(format!("{id}.log")).exists(),
        "no .log for a no-op"
    );
    assert!(
        !dir.join(format!("{id}.idx")).exists(),
        "no .idx for a no-op"
    );
    assert!(store.records().expect("empty catalog").records.is_empty());
    assert!(!dir.join(LAST_FILE).exists(), "a no-op never moves `last`");
    assert!(slipfork_world::run_log::resolve_run(&store.world_logs(), "last").is_err());
}
