//! Idle and heal for a head-matched world: one `up to date` row, or restart only down services.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::Heal;
use crate::daemon::Failure;
use crate::daemon::rpc::world_call_within;
use crate::ps::PsService;

const HEAL_INSPECT: Duration = Duration::from_secs(2);
const HEAL_RESTART: Duration = Duration::from_secs(30);
const HEAL_SETTLE: Duration = Duration::from_millis(250);
const HEAL_POLL: Duration = Duration::from_millis(50);
const HEAL_VERIFY: Duration = Duration::from_millis(1500);

/// Prints the one Idle row when every service is healthy, or restarts only the down services.
pub(crate) fn settle(plan: Heal, emit: &mut dyn FnMut(Value)) -> Result<Value, Failure> {
    let started = Instant::now();
    let mut latest = down_rows(&plan, "daemon-up-health", HEAL_INSPECT)?;
    if latest.is_empty() {
        let ms = millis(started);
        // Exactly one settled row for an unchanged world: no image row, no spinner frame.
        emit(json!({
            "event":"up-to-date","step":1,"total":1,"kind":"world","name":plan.world,
            "state":"up to date","elapsed_ms":0,"duration_ms":0,
        }));
        return Ok(ready(&plan.world, ms));
    }
    let label = format!("Verifying {}", plan.world);
    emit(json!({"event":"started","step":1,"total":1,"kind":"world","label":label}));
    let mut restart_error = None;
    for row in &latest {
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: format!("daemon-up-restart-{}", row.id),
            service: Some(row.id.clone()),
        };
        if let Err(error) = world_call_within(&plan.pods_dir, &request, HEAL_RESTART) {
            restart_error
                .get_or_insert_with(|| error.lines().next().unwrap_or("restart failed").to_owned());
        }
    }
    let deadline = Instant::now() + HEAL_VERIFY;
    let mut last_error = None;
    let mut healthy_since = None;
    while Instant::now() < deadline {
        match down_rows(&plan, "daemon-up-recheck", HEAL_INSPECT) {
            Ok(rows) if rows.is_empty() => {
                let since = *healthy_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= HEAL_SETTLE {
                    let ms = millis(started);
                    emit(
                        json!({"event":"finished","step":1,"total":1,"kind":"world","name":plan.world,"state":"running","label":format!("Recovered {}", plan.world),"detail":"services running","elapsed_ms":ms,"duration_ms":ms}),
                    );
                    return Ok(ready(&plan.world, ms));
                }
            }
            Ok(rows) => (latest, healthy_since) = (rows, None),
            Err(error) => (last_error, healthy_since) = (Some(error.message), None),
        }
        std::thread::sleep(HEAL_POLL);
    }
    let id = &latest.first().expect("heal keeps the initial down row").id;
    let detail = restart_error
        .or(last_error)
        .and_then(|error| error.lines().next().map(|line| format!(": {line}")))
        .unwrap_or_default();
    Err(Failure::new(
        "world-unhealthy",
        format!("service `{id}` is not running{detail}\nnext: slipfork logs {id}"),
    ))
}

fn ready(world: &str, elapsed_ms: u64) -> Value {
    json!({ "world": world, "url": format!("http://{world}.localhost"), "elapsed_ms": elapsed_ms })
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn down_rows(plan: &Heal, id: &str, timeout: Duration) -> Result<Vec<PsService>, Failure> {
    let request = Request::List {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
    };
    let reply = world_call_within(&plan.pods_dir, &request, timeout)
        .map_err(|error| world_failure(&plan.world, &error))?;
    let pods = slipfork_world::pod_statuses(&reply)
        .map_err(|error| world_failure(&plan.world, &error.to_string()))?;
    Ok(crate::daemon::lifecycle::ps::WorldView::new(
        &plan.project,
        &plan.world,
        plan.fork.as_deref(),
        plan.started_at_ms,
        plan.supervisor_pid,
        true,
        false,
        &pods,
    )
    .service_rows()
    .iter()
    .filter(|row| !row.orphaned && !row.running)
    .cloned()
    .collect())
}

fn world_failure(world: &str, error: &str) -> Failure {
    let message = format!("cannot inspect `{world}`: {error}\nnext: slipfork status");
    Failure::new("world-unhealthy", message)
}
