//! Algorithm-only DAG tests: wave ordering, dedupe, stamps, failure propagation, cycles.

use super::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FIXTURE: &str = r#"# forkfile: 0.1
image backend . {
  dockerfile Dockerfile
  pre-deploy: alembic upgrade head
  post-deploy: echo ready
  service api {
    port 8000
    run: @uv run app
  }
  service worker {
    run: @uv run worker
  }
}
"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(format!(
            "/var/tmp/slipfork-m339-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch project on disk");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn plan_root(scratch: &Scratch) -> WorldPlan {
    fs::write(scratch.path().join("uv.lock"), "version = 1\n").expect("write uv.lock");
    let doc = slipfork_forkfile::parse(FIXTURE).expect("fixture parses");
    world_plan(&doc, scratch.path())
}

fn collecting_sink() -> (Arc<Mutex<Vec<BootEvent>>>, impl Fn(BootEvent) + Sync) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let events = Arc::clone(&events);
        move |event: BootEvent| events.lock().expect("event sink").push(event)
    };
    (events, sink)
}

#[test]
fn deduplicates_the_at_tool_setup_and_orders_every_phase() {
    let scratch = Scratch::new("order");
    let plan = plan_root(&scratch);
    let setups: Vec<&BootStep> = plan
        .steps
        .iter()
        .filter(|step| step.kind == BootKind::Setup)
        .collect();
    assert_eq!(setups.len(), 1, "one deduplicated uv sync node: {setups:?}");
    assert_eq!(setups[0].command.as_deref(), Some("uv sync"));

    let position = |label: &str| {
        plan.steps
            .iter()
            .position(|step| step.label == label)
            .unwrap_or_else(|| panic!("missing step {label}: {:#?}", plan.steps))
    };
    let waves = plan.waves().expect("acyclic");
    let wave_of = |label: &str| {
        waves
            .iter()
            .position(|wave| wave.iter().any(|&index| plan.steps[index].label == label))
            .unwrap_or_else(|| panic!("missing wave for {label}"))
    };
    assert!(wave_of("Image backend") < wave_of("Setup backend: uv sync"));
    assert!(wave_of("Setup backend: uv sync") < wave_of("Hook backend:pre-deploy"));
    assert!(wave_of("Hook backend:pre-deploy") < wave_of("Service api"));
    assert!(wave_of("Service api") < wave_of("Hook backend:post-deploy"));
    assert_eq!(plan.failed_deploy.len(), 0);

    assert_eq!(waves[0], vec![position("Image backend")]);
    assert!(
        waves
            .iter()
            .any(|wave| wave.contains(&position("Setup backend: uv sync")))
    );
    assert!(
        waves
            .iter()
            .any(|wave| wave.contains(&position("Service api")))
    );
}

#[test]
fn stamps_skip_unchanged_steps_in_under_a_millisecond() {
    let scratch = Scratch::new("stamps");
    let plan = plan_root(&scratch);
    let stamps = StampStore::new(scratch.path());
    let runs = AtomicUsize::new(0);
    let (events, sink) = collecting_sink();
    let execute = |_index: usize, step: &BootStep| {
        if step.kind == BootKind::Setup && step.stamp.is_some() {
            runs.fetch_add(1, Ordering::SeqCst);
        }
        Ok(StepOutcome {
            detail: "ran".to_owned(),
        })
    };

    run_plan(&plan, &stamps, &execute, &sink).expect("first boot");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let stamp_file = fs::read_dir(scratch.path().join(".slipfork"))
        .expect("stamp directory")
        .count();
    assert_eq!(stamp_file, 1);

    let up_to_date = Mutex::new(0);
    let second = |event: BootEvent| {
        if let BootEvent::UpToDate { elapsed, .. } = &event {
            assert!(
                *elapsed < Duration::from_millis(1),
                "stamp check took {elapsed:?}"
            );
            *up_to_date.lock().expect("count") += 1;
        }
        events.lock().expect("events").push(event);
    };
    run_plan(&plan, &stamps, &execute, &second).expect("second boot");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "stamped step must not rerun"
    );
    assert_eq!(*up_to_date.lock().expect("count"), 1);

    // A rewritten stamp must rerun the step: a timer that never advances cannot pass this.
    let stamp_path = fs::read_dir(scratch.path().join(".slipfork"))
        .expect("stamp directory")
        .next()
        .expect("one stamp file")
        .expect("stamp entry")
        .path();
    fs::write(&stamp_path, "stale\n").expect("rewrite the stamp");
    let (_, third_sink) = collecting_sink();
    run_plan(&plan, &stamps, &execute, &third_sink).expect("third boot");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "a rewritten stamp must rerun the step"
    );
}

#[test]
fn a_failed_step_stops_later_waves_and_keeps_the_line() {
    let scratch = Scratch::new("failure");
    let plan = plan_root(&scratch);
    let stamps = StampStore::new(scratch.path());
    let (events, sink) = collecting_sink();
    let services = AtomicUsize::new(0);
    let execute = |_index: usize, step: &BootStep| {
        if step.phase == Some(HookPhase::PreDeploy) {
            return Err(BootFailure::new("exit 1"));
        }
        if step.kind == BootKind::Service {
            services.fetch_add(1, Ordering::SeqCst);
        }
        Ok(StepOutcome {
            detail: "ok".to_owned(),
        })
    };
    let error = run_plan(&plan, &stamps, &execute, &sink).expect_err("hook must fail");
    let message = error.to_string();
    assert!(message.contains("Hook backend:pre-deploy"), "{message}");
    assert!(message.contains("Forkfile:4"), "{message}");
    assert_eq!(
        services.load(Ordering::SeqCst),
        0,
        "services must not start"
    );
    assert!(
        events
            .lock()
            .expect("events")
            .iter()
            .any(|event| matches!(event, BootEvent::Failed { line: 4, .. }))
    );
}

#[test]
fn refuses_a_dependency_cycle() {
    let step = |label: &str, deps: Vec<usize>| BootStep {
        kind: BootKind::Setup,
        image: None,
        subject: label.to_owned(),
        phase: None,
        label: label.to_owned(),
        command: None,
        stamp: None,
        uses_env: false,
        line: 1,
        deps,
    };
    let plan = WorldPlan {
        steps: vec![step("a", vec![1]), step("b", vec![0])],
        ..WorldPlan::default()
    };
    let error = plan.waves().expect_err("cycle must be refused");
    assert!(matches!(error, Error::BootCycle { .. }));
    assert!(error.to_string().contains("a"), "{error}");
}

#[test]
fn stamp_store_round_trips_the_digest_atomically() {
    let scratch = Scratch::new("store");
    let store = StampStore::new(scratch.path());
    let stamp = StepStamp {
        key: "setup-abc.stamp".to_owned(),
        digest: "deadbeef".to_owned(),
    };
    assert!(!store.is_current(&stamp));
    store.record(&stamp).expect("record");
    assert!(store.is_current(&stamp));
    let recorded =
        fs::read_to_string(scratch.path().join(".slipfork/setup-abc.stamp")).expect("read stamp");
    assert_eq!(recorded.trim(), "deadbeef");
}
