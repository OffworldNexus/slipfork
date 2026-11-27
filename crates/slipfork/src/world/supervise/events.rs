//! Typed `BootEvent` -> NDJSON projection for the supervisor's stdout stream.

use serde_json::{Value, json};
use slipfork_world::{BootEvent, BootState};

use crate::ui::report::{Counter, Outcome};
use crate::world::runtime::frames::failure_facts;

pub(super) fn boot_event_json(event: &BootEvent, total: usize) -> Value {
    match event {
        BootEvent::Plan { rows } => json!({
            "event": "plan",
            "rows": rows
                .iter()
                .map(|row| json!({ "kind": row.kind.as_str(), "name": row.name }))
                .collect::<Vec<_>>(),
            "total": rows.len(),
        }),
        BootEvent::Started {
            step,
            total,
            kind,
            name,
            run_id,
        } => json!({
            "event": "started",
            "step": step + 1,
            "total": total,
            "kind": kind.as_str(),
            "name": name,
            "state": "running",
            "outcome": Outcome::Running,
            "run_id": run_id,
        }),
        BootEvent::Finished {
            kind,
            name,
            elapsed,
            state,
            detail,
            run_id,
            ..
        } => json!({
            "event": "finished",
            "kind": kind.as_str(),
            "name": name,
            "state": state.as_str(),
            "detail": detail,
            "elapsed_ms": elapsed.as_millis(),
            "duration_ms": elapsed.as_millis(),
            "run_id": run_id,
        }),
        BootEvent::UpToDate {
            kind,
            name,
            elapsed,
            ..
        } => json!({
            "event": "up-to-date",
            "kind": kind.as_str(),
            "name": name,
            "state": BootState::UpToDate.as_str(),
            "detail": "up to date",
            "elapsed_ms": elapsed.as_millis(),
            "duration_ms": elapsed.as_millis(),
        }),
        BootEvent::Failed {
            step,
            kind,
            name,
            line,
            message,
            remedy,
            elapsed,
            run_id,
            command,
            ..
        } => {
            let mut event = json!({
                "event": "failed",
                "kind": kind.as_str(),
                "name": name,
                "state": BootState::Failed.as_str(),
                "line": line,
                "message": message,
                "elapsed_ms": elapsed.as_millis(),
                "duration_ms": elapsed.as_millis(),
                "run_id": run_id,
                "command": command,
            });
            if let Some(remedy) = remedy {
                event["remedy"] = json!(remedy);
            }
            // The producing path owns the typed envelope: the outer catch only merges the
            // store-derived phase, reason, and exit code, never a second projection.
            let at = (total > 0 && *step < total).then(|| Counter {
                index: *step + 1,
                total,
            });
            failure_facts(&mut event, None, command.as_deref(), at);
            event
        }
    }
}

/// The plan index a boot event names, when it names one.
pub(super) fn step_index(event: &BootEvent) -> Option<usize> {
    match event {
        BootEvent::Started { step, .. } | BootEvent::Failed { step, .. } => Some(*step),
        _ => None,
    }
}
