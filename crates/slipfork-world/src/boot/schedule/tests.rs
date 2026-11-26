use std::sync::{Arc, Mutex};

use super::{BootEvent, BootScheduler, EventSink, StepExecutor};
use crate::boot::{BootKind, BootState, BootStep, StampStore, StepOutcome, WorldPlan};

fn service_step(id: &str) -> BootStep {
    BootStep {
        kind: BootKind::Service,
        image: None,
        subject: id.to_owned(),
        phase: None,
        label: format!("service {id}"),
        command: Some("true".to_owned()),
        stamp: None,
        uses_env: false,
        line: 1,
        deps: Vec::new(),
    }
}

/// The plan frame is the first event and names every planned step before any worker runs.
#[test]
fn the_plan_frame_precedes_the_first_started_and_lists_every_step() {
    let dir = tempfile::Builder::new()
        .prefix("slipfork-plan-frame")
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory");
    let plan = WorldPlan {
        steps: vec![service_step("api"), service_step("worker")],
        ..WorldPlan::default()
    };
    let seen: Arc<Mutex<Vec<BootEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<EventSink> = {
        let seen = Arc::clone(&seen);
        Arc::new(move |event: BootEvent| {
            seen.lock().expect("event lock").push(event);
        })
    };
    let executor: Arc<StepExecutor> = Arc::new(|_: usize, _: &BootStep| {
        Ok(StepOutcome {
            state: BootState::Ready,
            detail: String::new(),
            run_id: None,
        })
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build the test runtime");
    let scheduler = Arc::new(BootScheduler::new(
        runtime.handle().clone(),
        StampStore::gated(dir.path(), false),
        executor,
        sink,
    ));
    scheduler.run(&plan).expect("an acyclic plan boots");

    let seen = seen.lock().expect("event lock");
    let BootEvent::Plan { rows } = &seen[0] else {
        panic!("the first event must be `Plan`, got {:?}", seen[0]);
    };
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["api", "worker"]);
    assert!(
        seen[1..]
            .iter()
            .any(|event| matches!(event, BootEvent::Started { .. })),
        "the plan must precede a real `Started`"
    );
    assert!(
        seen[1..].iter().all(|event| matches!(
            event,
            BootEvent::Started { .. } | BootEvent::Finished { .. }
        )),
        "the plan is announced exactly once: {:?}",
        &seen[1..]
    );
}
