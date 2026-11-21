//! Algorithm-only DAG tests: edge release, no-barrier overlap, cancellation, and epoch gating.
//!
//! No sleeps and no wall-clock assertions: the executor itself proves when a node was allowed to
//! run, and every stamp lives on real disk under `/var/tmp`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BootEvent, BootFailure, BootKind, BootScheduler, BootState, BootStep, EpochState, EventSink,
    StampStore, StepExecutor, StepOutcome, StepStamp, WorldPlan, WorldStamp, WorldStampStore,
    plan_epoch, write_pg_checkpoint,
};

fn lab(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let dir = Path::new("/var/tmp").join(format!(
        "slipfork-boot-dag-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create the lab dir");
    dir
}

fn step(label: &str, deps: Vec<usize>) -> BootStep {
    BootStep {
        kind: BootKind::Setup,
        image: None,
        subject: label.to_owned(),
        phase: None,
        label: label.to_owned(),
        command: Some("true".to_owned()),
        stamp: None,
        uses_env: false,
        line: 1,
        deps,
    }
}

fn plan(steps: Vec<BootStep>) -> WorldPlan {
    WorldPlan {
        steps,
        ..WorldPlan::default()
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build the test runtime")
}

fn collector() -> Arc<EventSink> {
    Arc::new(|_: BootEvent| {})
}

fn done(detail: &str) -> StepOutcome {
    StepOutcome {
        state: BootState::Ready,
        detail: detail.to_owned(),
    }
}

#[test]
fn dependents_start_only_after_every_edge() {
    let dir = lab("release");
    let plan = plan(vec![
        step("a", Vec::new()),
        step("b", Vec::new()),
        step("c", vec![0, 1]),
    ]);
    let completed = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<StepExecutor> = {
        let completed = Arc::clone(&completed);
        let seen = Arc::clone(&seen);
        Arc::new(move |index: usize, _: &BootStep| {
            if index == 2 {
                assert_eq!(
                    completed.load(Ordering::SeqCst),
                    2,
                    "`c` must be released only after both upstream edges"
                );
            }
            completed.fetch_add(1, Ordering::SeqCst);
            seen.lock().expect("seen lock").push(index);
            Ok(done("ok"))
        })
    };
    let runtime = runtime();
    let scheduler = Arc::new(BootScheduler::new(
        runtime.handle().clone(),
        StampStore::gated(&dir, true),
        executor,
        collector(),
    ));
    scheduler.run(&plan).expect("an acyclic plan boots");
    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 3);
    assert_eq!(*seen.last().expect("a node ran"), 2, "`c` runs last");
    assert!(seen[..2].contains(&0) && seen[..2].contains(&1));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_dependent_runs_before_an_unrelated_slow_sibling_finishes() {
    let dir = lab("barrier");
    let plan = plan(vec![
        step("fast", Vec::new()),
        step("slow", Vec::new()),
        step("dependent", vec![0]),
    ]);
    // `slow` blocks on a channel; `dependent` proves it ran first and only then releases it. A
    // wave-barrier regression deadlocks and fails closed through the receive timeout.
    let slow_done = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let executor: Arc<StepExecutor> = {
        let slow_done = Arc::clone(&slow_done);
        let release_rx = Arc::clone(&release_rx);
        Arc::new(move |index: usize, _: &BootStep| match index {
            1 => {
                let released = release_rx
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(30));
                if released == Err(RecvTimeoutError::Timeout) {
                    return Err(BootFailure::new(
                        "barrier: `dependent` never ran while `slow` was blocked",
                    ));
                }
                slow_done.store(true, Ordering::SeqCst);
                Ok(done("slow"))
            }
            2 => {
                if slow_done.load(Ordering::SeqCst) {
                    return Err(BootFailure::new(
                        "barrier: `dependent` ran only after its unrelated sibling finished",
                    ));
                }
                release_tx.send(()).expect("release the slow sibling");
                Ok(done("dependent"))
            }
            _ => Ok(done("fast")),
        })
    };
    let runtime = runtime();
    let scheduler = Arc::new(BootScheduler::new(
        runtime.handle().clone(),
        StampStore::gated(&dir, true),
        executor,
        collector(),
    ));
    scheduler
        .run(&plan)
        .expect("no phase barrier holds a released dependent back");
    assert!(
        slow_done.load(Ordering::SeqCst),
        "the slow sibling finished"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn first_failure_cancels_not_yet_started_nodes() {
    let dir = lab("cancel");
    let plan = plan(vec![step("root", Vec::new()), step("child", vec![0])]);
    let ran = Arc::new(AtomicUsize::new(0));
    let executor: Arc<StepExecutor> = {
        let ran = Arc::clone(&ran);
        Arc::new(move |index: usize, _: &BootStep| {
            if index == 0 {
                return Err(BootFailure::new("boom"));
            }
            ran.fetch_add(1, Ordering::SeqCst);
            Ok(done("ok"))
        })
    };
    let runtime = runtime();
    let scheduler = Arc::new(BootScheduler::new(
        runtime.handle().clone(),
        StampStore::gated(&dir, true),
        executor,
        collector(),
    ));
    let error = scheduler.run(&plan).expect_err("the root node fails");
    assert!(matches!(error, slipfork_world::Error::Boot { .. }));
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a not-yet-started dependent is cancelled"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn epoch_pair_gates_stamp_reuse() {
    let dir = lab("epoch");
    let world = dir.join("world");
    let pg = dir.join("world.pg.pg");
    fs::create_dir_all(&world).expect("world dir");
    fs::create_dir_all(&pg).expect("pg dir");
    let store = WorldStampStore::new(&world);
    store
        .save(&WorldStamp {
            world_epoch: "e1".to_owned(),
            ..WorldStamp::default()
        })
        .expect("write world stamp");
    write_pg_checkpoint(&pg, "e1").expect("write pg stamp");
    assert_eq!(
        store.verify(Some(&pg)),
        EpochState::Matched("e1".to_owned())
    );
    write_pg_checkpoint(&pg, "e2").expect("rewrite pg stamp");
    assert_eq!(store.verify(Some(&pg)), EpochState::Mismatched);

    let project = dir.join("project");
    fs::create_dir_all(&project).expect("project dir");
    let digest = StepStamp {
        key: "setup-x.stamp".to_owned(),
        digest: "digest".to_owned(),
    };
    let trusted = StampStore::gated(&project, true);
    trusted.record(&digest).expect("record a trusted stamp");
    assert!(trusted.is_current(&digest));
    let gated = StampStore::gated(&project, false);
    assert!(
        !gated.is_current(&digest),
        "a mismatched epoch must force the hook to re-run"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn plan_epoch_is_stable_for_one_shape_and_changes_with_the_command() {
    let first = plan(vec![step("a", Vec::new()), step("b", vec![0])]);
    assert_eq!(plan_epoch(&first), plan_epoch(&first.clone()));
    let mut changed = first.clone();
    changed.steps[0].command = Some("false".to_owned());
    assert_ne!(plan_epoch(&first), plan_epoch(&changed));
}
