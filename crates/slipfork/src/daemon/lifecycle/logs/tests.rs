use std::io::Write;
use std::sync::Mutex;

use slipfork_world::WorldName;
use slipfork_world::run_log::RunCapture;

use super::{LogStream, RunIndex};

/// A catalog run replays even when no `<world_logs>/<id>.log` exists, so a failed build or
/// hook survives `fail_boot` forgetting the world.
#[test]
fn a_catalog_run_replays_without_a_world_capture() {
    let root = tempfile::Builder::new()
        .prefix("logs-catalog")
        .tempdir_in("/var/tmp")
        .expect("disk temp directory");
    let store = slipfork_world::LogStore::at(
        root.path().to_path_buf(),
        WorldName::parse("demo").expect("valid world"),
    );
    let project = root.path().join("demo");
    let mut capture = RunCapture::begin(&store, &project, "image", "app", "dockerfile .")
        .expect("begin a real run capture");
    capture
        .append_stream(slipfork_world::Stream::Stdout, "failed-run-marker")
        .expect("append the line");
    let run_id = capture.run_id().to_owned();
    capture.finish("failed", 9).expect("finish the run");
    assert!(!store.world_logs().join("app.log").exists());
    // One old-schema line is counted and skipped instead of bricking the catalog.
    let mut index_file = std::fs::OpenOptions::new()
        .append(true)
        .open(store.index())
        .expect("open the catalog");
    writeln!(index_file, "{{\"legacy\":true}}").expect("append an old-schema line");
    drop(index_file);
    let index = RunIndex::load(&store).expect("load the catalog once");
    assert_eq!(
        store.records().expect("read the catalog").skipped,
        1,
        "the old-schema line is counted"
    );

    let events = Mutex::new(Vec::new());
    let outcome = LogStream::captures(
        store.clone(),
        vec!["app".to_owned()],
        true,
        false,
        None,
        None,
    )
    .with_catalog(index)
    .run(
        &mut |event| {
            events.lock().expect("record the event").push(event);
            true
        },
        &mut || true,
    )
    .expect("the catalog fallback replays");
    assert_eq!(outcome["closed"], false);
    let events = events.into_inner().expect("read the events");
    assert!(
        events
            .iter()
            .any(|event| event["line"] == "failed-run-marker" && event["pod"] == "app"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| event["kind"] == "roster"),
        "the replay announces its roster first: {events:?}"
    );
    assert_eq!(run_id.len(), slipfork_world::run_log::RUN_ID_LEN);

    let runs = LogStream::run_catalog(store.clone())
        .run(&mut |_| true, &mut || true)
        .expect("list the catalog");
    assert_eq!(runs["runs"], 1, "{runs}");
    assert_eq!(
        runs["skipped"], 1,
        "the old-schema line is reported: {runs}"
    );

    let refused = LogStream::captures(store, vec!["missing".to_owned()], true, false, None, None)
        .run(&mut |_| true, &mut || true);
    assert!(
        refused.is_err(),
        "an unknown target still fails closed: {refused:?}"
    );
}
