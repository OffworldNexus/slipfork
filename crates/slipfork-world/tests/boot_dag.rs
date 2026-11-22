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
    BootEvent, BootFailure, BootGraph, BootKind, BootScheduler, BootState, BootStep, EpochState,
    EventSink, StampStore, StepExecutor, StepOutcome, StepStamp, WorldPlan, WorldStamp,
    WorldStampStore, plan_epoch, write_pg_checkpoint,
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

#[test]
fn fork_world_stamp_persists_and_gates_before_start_and_pre_deploy_hooks() {
    let dir = lab("fork-hooks");
    let mut before = step("Hook app:before-start", Vec::new());
    before.kind = BootKind::Setup;
    before.image = Some("app".to_owned());
    before.subject = "app".to_owned();
    before.phase = Some(slipfork_world::HookPhase::BeforeStart);
    before.command = Some("echo before".to_owned());

    let mut pre = step("Hook api:pre-deploy", vec![0]);
    pre.kind = BootKind::Hook;
    pre.image = Some("app".to_owned());
    pre.subject = "api".to_owned();
    pre.phase = Some(slipfork_world::HookPhase::PreDeploy);
    pre.command = Some("echo pre".to_owned());

    let world_plan = plan(vec![before, pre]);
    let epoch = plan_epoch(&world_plan);
    let checkpoint = WorldStamp::checkpoint(&world_plan, epoch, Default::default());
    assert_eq!(checkpoint.before_start.len(), 1);
    assert_eq!(checkpoint.pre_deploy.len(), 1);

    let runs = Arc::new(AtomicUsize::new(0));
    let executor: Arc<StepExecutor> = {
        let runs = Arc::clone(&runs);
        Arc::new(move |_: usize, _: &BootStep| {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(done("ran"))
        })
    };
    let rt = runtime();
    let matched = Arc::new(BootScheduler::new(
        rt.handle().clone(),
        StampStore::for_fork(&dir, true, Some(checkpoint.clone())),
        Arc::clone(&executor),
        collector(),
    ));
    matched.run(&world_plan).expect("matched fork skips hooks");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "matching epoch + hook digest must skip in 0ms without calling executor"
    );

    let mismatched = Arc::new(BootScheduler::new(
        rt.handle().clone(),
        StampStore::for_fork(&dir, false, Some(checkpoint)),
        executor,
        collector(),
    ));
    mismatched
        .run(&world_plan)
        .expect("mismatched epoch re-runs hooks");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "epoch mismatch must re-run both hooks"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The shipped eight-component `support/demo` plan, compiled from the real `Forkfile` on disk.
fn demo_plan() -> WorldPlan {
    let dir = lab("demo-plan");
    let forkfile = dir.join("Forkfile");
    fs::write(&forkfile, include_str!("../../../support/demo/Forkfile"))
        .expect("write the shipped demo Forkfile into the lab");
    let source = fs::read_to_string(&forkfile).expect("read the shipped demo Forkfile");
    let checked = slipfork_forkfile::compile(&source).expect("the shipped demo Forkfile compiles");
    let plan = slipfork_world::world_plan(&checked.document, &dir).expect("plan the shipped demo");
    let _ = fs::remove_dir_all(&dir);
    plan
}

fn index_of(plan: &WorldPlan, subject: &str) -> usize {
    plan.steps
        .iter()
        .position(|step| step.subject == subject)
        .unwrap_or_else(|| panic!("`{subject}` must be a node"))
}

#[test]
fn current_epoch_services_carry_no_resource_edges_and_skip_the_pre_deploy_barrier() {
    let plan = demo_plan();
    let pg = index_of(&plan, "pg");
    let service = |id: &str| {
        plan.steps
            .iter()
            .position(|step| step.kind == BootKind::Service && step.subject == id)
            .unwrap_or_else(|| panic!("`{id}` must be a service node"))
    };
    let api = service("api");
    let worker = service("worker");
    let backend = index_of(&plan, "backend");
    let pre = plan
        .steps
        .iter()
        .position(|step| step.phase == Some(slipfork_world::HookPhase::PreDeploy))
        .expect("a pre-deploy node");

    let rows: Vec<&str> = plan
        .steps
        .iter()
        .filter(|step| matches!(step.kind, BootKind::Resource | BootKind::Service))
        .map(|step| step.subject.as_str())
        .collect();
    assert_eq!(
        rows,
        [
            "pg", "cache", "uploads", "assets", "api", "worker", "front", "mailpit"
        ],
        "the shipped demo must boot exactly eight rows"
    );
    let resources: Vec<usize> = (0..plan.steps.len())
        .filter(|&index| plan.steps[index].kind == BootKind::Resource)
        .collect();
    assert_eq!(resources.len(), 4, "the shipped demo has four resources");
    assert!(
        resources
            .iter()
            .all(|dep| !plan.steps[api].deps.contains(dep)),
        "a service node must not wait for resources: {:?}",
        plan.steps[api].deps
    );
    assert_eq!(
        plan.gated.get(&pre).map(Vec::as_slice),
        Some(resources.as_slice()),
        "the pre-deploy resource barrier must stay explicit, not silently dropped"
    );
    assert!(
        resources
            .iter()
            .all(|dep| !plan.steps[pre].deps.contains(dep)),
        "resources gate `pre-deploy` only through the stamped edge"
    );
    assert_eq!(
        plan.steps[worker].deps,
        vec![backend],
        "`worker` carries only its image edge"
    );
    let graph =
        BootGraph::with_gated(&plan.steps, &plan.gated).expect("the shipped plan is acyclic");
    assert!(
        graph.downstream(backend).contains(&api) && graph.downstream(backend).contains(&worker),
        "the shared `backend` image releases both services"
    );
    assert!(
        !graph.downstream(pg).contains(&api),
        "the downstream accessor must expose releases, not upstream deps"
    );

    let dir = lab("conditional-current");
    let checkpoint = WorldStamp::checkpoint(&plan, plan_epoch(&plan), Default::default());
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let up_to_date = Arc::new(Mutex::new(Vec::new()));
    let seen_sink = Arc::clone(&up_to_date);
    let sink: Arc<EventSink> = Arc::new(move |event: BootEvent| {
        if let BootEvent::UpToDate { step, .. } = event {
            seen_sink.lock().expect("seen lock").push(step);
        }
    });
    let executor: Arc<StepExecutor> = {
        let release_rx = Arc::clone(&release_rx);
        Arc::new(move |index: usize, _: &BootStep| {
            if index == pg {
                let released = release_rx
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(10));
                if released.is_err() {
                    return Err(BootFailure::new(
                        "the service never started while `pg` was still booting",
                    ));
                }
            }
            if index == api {
                release_tx.send(()).expect("release postgres");
            }
            Ok(done("ok"))
        })
    };
    let rt = runtime();
    let scheduler = Arc::new(BootScheduler::new(
        rt.handle().clone(),
        StampStore::for_fork(&dir, true, Some(checkpoint)),
        executor,
        sink,
    ));
    scheduler
        .run(&plan)
        .expect("a current epoch releases the service in parallel with postgres");
    assert!(
        up_to_date.lock().expect("seen lock").contains(&pre),
        "the current pre-deploy stamp must resolve in 0ms without touching resources"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stale_epoch_gates_the_pre_deploy_on_every_resource() {
    let plan = demo_plan();
    let pre = plan
        .steps
        .iter()
        .position(|step| step.phase == Some(slipfork_world::HookPhase::PreDeploy))
        .expect("a pre-deploy node");
    let resources: Vec<usize> = (0..plan.steps.len())
        .filter(|&index| plan.steps[index].kind == BootKind::Resource)
        .collect();

    let dir = lab("conditional-stale");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<StepExecutor> = {
        let seen = Arc::clone(&seen);
        Arc::new(move |index: usize, _: &BootStep| {
            if index == pre {
                let done: Vec<usize> = seen.lock().expect("seen lock").clone();
                for &resource in &resources {
                    if !done.contains(&resource) {
                        return Err(BootFailure::new(
                            "a stale `pre-deploy` ran before every backing resource was ready",
                        ));
                    }
                }
            } else {
                seen.lock().expect("seen lock").push(index);
            }
            Ok(done("ok"))
        })
    };
    let rt = runtime();
    let scheduler = Arc::new(BootScheduler::new(
        rt.handle().clone(),
        StampStore::gated(&dir, false),
        executor,
        collector(),
    ));
    scheduler
        .run(&plan)
        .expect("a stale epoch awaits pg/redis/s3 before `pre-deploy`");
    let _ = fs::remove_dir_all(&dir);
}
