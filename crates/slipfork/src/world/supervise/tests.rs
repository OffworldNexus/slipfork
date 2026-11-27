use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::{FeatureEdit, FeatureSet};
use slipfork_world::{
    BootEvent, BootKind, BootStep, HookPhase, PlanRow, StepStamp, WorldPlan, WorldStamp,
    WorldStampStore, plan_epoch, write_pg_checkpoint,
};

use super::events::boot_event_json;
use super::stamps_for;
use crate::world::runtime::capture::RunTarget;

/// The conditional-stamp key a `pre-deploy when changed(uv.lock)` hook records.
const LOCK_STAMP: &str = "pre-deploy-lock";

fn temp(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory")
}

fn hook_plan(command: &str, lock: Option<&str>) -> WorldPlan {
    WorldPlan {
        steps: vec![BootStep {
            kind: BootKind::Hook,
            image: None,
            subject: "api".to_owned(),
            phase: Some(HookPhase::PreDeploy),
            label: "hook api:pre-deploy".to_owned(),
            command: Some(command.to_owned()),
            stamp: lock.map(|digest| StepStamp {
                key: LOCK_STAMP.to_owned(),
                digest: digest.to_owned(),
            }),
            uses_env: false,
            line: 7,
            deps: Vec::new(),
        }],
        ..WorldPlan::default()
    }
}

fn fixture_features() -> (FeatureSet, FeatureSet) {
    let checked = slipfork_forkfile::compile("# forkfile: 0.1\n\nfeatures {\n  dev\n}\n")
        .expect("compile minimal feature fixture");
    let dev = checked.document.default_features();
    let prod = checked
        .document
        .resolve_features(false, &[FeatureEdit::Disable("dev".to_owned())])
        .expect("resolve `-F -dev`");
    (dev, prod)
}

/// Writes a real world checkpoint plus the paired PG sibling `slipfork fork` also copies.
fn checkpoint(root: &Path, plan: &WorldPlan, features: &FeatureSet) -> (WorldStampStore, PathBuf) {
    let world_dir = root.join("worlds/demo");
    let pg_clone = root.join("worlds/demo.pg.pg");
    fs::create_dir_all(&world_dir).expect("create the world dir");
    fs::create_dir_all(&pg_clone).expect("create the PG clone");
    let store = WorldStampStore::new(&world_dir);
    let epoch = plan_epoch(plan, features, &BTreeMap::new());
    store
        .save(&WorldStamp::checkpoint(plan, epoch.clone()))
        .expect("write the world stamp");
    write_pg_checkpoint(&pg_clone, &epoch).expect("write the PG stamp");
    (store, pg_clone)
}

/// A warm root world reuses its checkpoint; a changed `Forkfile`, features, or lockfile re-runs.
#[test]
fn a_paired_world_stamp_skips_pre_deploy_and_any_change_reruns_it() {
    let root = temp("slipfork-warm-stamp");
    let project = root.path().join("demo");
    fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");
    let (dev, prod) = fixture_features();

    let warm = hook_plan("uv run seed", None);
    let (store, pg_clone) = checkpoint(root.path(), &warm, &dev);
    let trusted = |plan: &WorldPlan, features: &FeatureSet| {
        store
            .trusted_stamp(plan, features, &BTreeMap::new(), [pg_clone.as_path()])
            .is_some()
    };
    assert!(
        trusted(&warm, &dev),
        "an unchanged plan + feature set + paired PG stamp is trusted"
    );
    assert!(
        !trusted(&warm, &prod),
        "disabling `dev` must invalidate the checkpoint even when step commands match"
    );
    let stamps = stamps_for(&project, &store, trusted(&warm, &dev));
    assert!(
        stamps.is_step_current(&warm.steps, 0),
        "a warm `up` must print `hook api:pre-deploy up to date`"
    );
    let prod_stamps = stamps_for(&project, &store, trusted(&warm, &prod));
    assert!(
        !prod_stamps.is_step_current(&warm.steps, 0),
        "a `-F -dev` boot must re-run `pre-deploy` instead of trusting a `dev` stamp"
    );

    // An edited `Forkfile` changes `world_epoch`, so nothing is trusted and the hook runs.
    let edited = hook_plan("uv run seed --fresh", None);
    assert!(
        !trusted(&edited, &dev),
        "an edited Forkfile invalidates the checkpoint"
    );
    let stamps = stamps_for(&project, &store, trusted(&edited, &dev));
    assert!(
        !stamps.is_step_current(&edited.steps, 0),
        "an edited Forkfile must re-run `pre-deploy`"
    );

    // A lockfile edit keeps the epoch but makes the recorded conditional digest stale.
    let lock = project.join(".slipfork").join(LOCK_STAMP);
    fs::write(&lock, "uv.lock@aaa\n").expect("write the lockfile stamp");
    let pinned = hook_plan("uv run seed", Some("uv.lock@aaa"));
    let (pinned_store, pinned_pg) = checkpoint(root.path(), &pinned, &dev);
    let pinned_trusted = |plan: &WorldPlan| {
        pinned_store
            .trusted_stamp(plan, &dev, &BTreeMap::new(), [pinned_pg.as_path()])
            .is_some()
    };
    assert!(pinned_trusted(&pinned));
    let stamps = stamps_for(&project, &pinned_store, pinned_trusted(&pinned));
    assert!(
        stamps.is_step_current(&pinned.steps, 0),
        "an unchanged lockfile stays `up to date`"
    );

    let relocked = hook_plan("uv run seed", Some("uv.lock@bbb"));
    assert_eq!(
        plan_epoch(&relocked, &dev, &BTreeMap::new()),
        plan_epoch(&pinned, &dev, &BTreeMap::new()),
        "same plan shape, same epoch"
    );
    let stamps = stamps_for(&project, &pinned_store, pinned_trusted(&relocked));
    assert!(
        !stamps.is_step_current(&relocked.steps, 0),
        "a changed lockfile must re-run `pre-deploy`"
    );
}

/// A step that errors before printing still owns an eager capture and a catalog record.
#[test]
fn an_eagerly_opened_run_records_a_capture_before_the_step_prints_anything() {
    let root = temp("supervise-run-target");
    let store = slipfork_world::LogStore::at(
        root.path().join("store"),
        slipfork_world::WorldName::parse("demo").expect("valid world name"),
    );
    let project = root.path().join("apps/demo");
    let mut target = RunTarget::open(&store, &project, "setup", "backend", "uv sync")
        .expect("open the capture before the step runs");

    let running = store.records().expect("read the catalog").records;
    assert_eq!(
        running.len(),
        1,
        "begin appends one `running` record: {running:?}"
    );
    let run_id = running[0].run_id.clone();
    assert_eq!(running[0].state, "running");
    assert!(
        store.capture(&run_id).is_file(),
        "the capture exists before the first printed line"
    );

    // The runtime writes this synthetic line precisely so the capture is never empty; the test
    // replays whatever the production formatter produced.
    let exit = crate::world::runtime::exit_line("uv sync", 2);
    target.append_stream(slipfork_world::Stream::Stdout, &exit);
    assert_eq!(target.finish("failed", 7).as_deref(), Some(run_id.as_str()));

    let mut reader = slipfork_world::pod_log::read_range(&store.run_dir(), &run_id, None, None)
        .expect("replay the failed capture");
    let mut lines = Vec::new();
    let _ = reader.read_lines(&mut |line| lines.push(line.text.clone()));
    reader.flush(&mut |line| lines.push(line.text.clone()));
    assert!(
        lines.iter().any(|line| line.contains(&exit)),
        "the failed capture replays the produced text `{exit}`: {lines:?}"
    );

    let finished = store.records().expect("read the catalog").records;
    assert_eq!(finished.len(), 1, "one record per run id: {finished:?}");
    assert_eq!(finished[0].state, "failed");
    assert_eq!(finished[0].duration_ms, 7);
}

/// The `plan` frame is the first NDJSON line, lists every row, and carries zero escapes.
#[test]
fn the_plan_frame_comes_first_and_stays_ansi_free() {
    let frames = [
        boot_event_json(
            &BootEvent::Plan {
                rows: vec![
                    PlanRow {
                        kind: BootKind::Image,
                        name: "base".to_owned(),
                    },
                    PlanRow {
                        kind: BootKind::Service,
                        name: "api".to_owned(),
                    },
                ],
            },
            2,
        ),
        boot_event_json(
            &BootEvent::Started {
                step: 0,
                total: 2,
                kind: BootKind::Image,
                name: "base".to_owned(),
                run_id: None,
            },
            2,
        ),
    ];
    let lines: Vec<String> = frames.iter().map(serde_json::Value::to_string).collect();
    assert!(lines[0].starts_with("{\"event\":\"plan\""), "{}", lines[0]);
    assert!(
        lines[0].contains("\"kind\":\"image\",\"name\":\"base\"")
            && lines[0].contains("\"kind\":\"service\",\"name\":\"api\""),
        "{}",
        lines[0]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&lines[0]).expect("the plan parses")["total"],
        2
    );
    assert!(lines[1].contains("\"event\":\"started\""));
    for line in &lines {
        assert!(!line.contains('\u{1b}'), "--json stays pure: {line:?}");
    }
}
