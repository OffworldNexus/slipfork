//! Typed NDJSON frames for live build rows and boot failures: shapes only, zero rendering.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_image::StepEvent;

use crate::ui::report::{Counter, FailureFacts};
use slipfork_world::run_log::RunRecord;
use slipfork_world::snapshot::TailPhase;

/// Environment key the daemon uses to hand one boot narration line to `__supervise`.
///
/// Mirrors `crate::daemon::lifecycle::up::append::UP_NOTE_ENV`; each side of the process boundary
/// states its own literal because the two module trees are private to `world` and `daemon`.
pub(crate) const UP_NOTE_ENV: &str = "SLIPFORK_UP_NOTE";

/// Frame-kind marker for the one log-rotation note; the frame vocabulary owns every kind name.
pub(crate) const ROTATION_NOTE: &str = "log-rotation";

impl StepPin<'_> {
    /// Parses a `step` frame's `phase` label through the one wire vocabulary owned here.
    #[cfg(test)]
    pub(crate) fn tail_phase(value: &str) -> Option<TailPhase> {
        tail_phase_from_wire(value)
    }

    /// The label a typed tail phase writes on a `step` frame.
    #[allow(dead_code)]
    pub(crate) const fn tail_label(phase: TailPhase) -> &'static str {
        tail_wire_label(phase)
    }
}

/// The JSON `phase` label one tail frame writes (`cloning` … `publishing`).
const fn tail_wire_label(phase: TailPhase) -> &'static str {
    match phase {
        TailPhase::Cloning => "cloning",
        TailPhase::Executing => "executing",
        TailPhase::Sealing => "sealing",
        TailPhase::Recording => "recording",
        TailPhase::Publishing => "publishing",
    }
}

/// Parses the JSON `phase` label back into the typed phase.
#[cfg(test)]
fn tail_phase_from_wire(value: &str) -> Option<TailPhase> {
    match value {
        "cloning" => Some(TailPhase::Cloning),
        "executing" => Some(TailPhase::Executing),
        "sealing" => Some(TailPhase::Sealing),
        "recording" => Some(TailPhase::Recording),
        "publishing" => Some(TailPhase::Publishing),
        _ => None,
    }
}

/// Identity a `log` frame repeats from the step that produced it.
pub(crate) struct StepPin<'a> {
    /// 1-based boot step index.
    pub step: usize,
    /// Boot steps in the plan.
    pub total: usize,
    /// Step label (`hook api:pre-deploy`).
    pub label: &'a str,
    /// Bare node name (`api`).
    pub name: &'a str,
    /// Image id while a `Dockerfile` builds, otherwise `None`.
    pub image: Option<&'a str>,
    /// Instruction index inside that image's `Dockerfile`.
    pub index: Option<usize>,
}

/// One `log` frame: a child line with its stream, timestamp, and step identity.
pub(crate) fn log_frame(pin: &StepPin<'_>, stream: &str, line: &str) -> Value {
    let mut frame = json!({
        "event": "log",
        "line": line,
        "step": pin.step,
        "total": pin.total,
        "label": pin.label,
        "name": pin.name,
        "stream": stream,
        "ts_ms": now_ms(),
    });
    if let Some(image) = pin.image {
        frame["image"] = json!(image);
    }
    if let Some(index) = pin.index {
        frame["index"] = json!(index);
    }
    frame
}

/// One `note` frame: engine narration (seal/cache/digest bookkeeping) pinned to its step.
///
/// It is never step output, so no renderer may paint it as a gutter line or write it to a capture;
/// `-v` shows it as one dim row and nothing else does.
pub(crate) fn note_frame(pin: &StepPin<'_>, text: &str) -> Value {
    let mut frame = json!({
        "event": "note",
        "text": text,
        "step": pin.step,
        "total": pin.total,
        "label": pin.label,
        "name": pin.name,
        "ts_ms": now_ms(),
    });
    if let Some(image) = pin.image {
        frame["image"] = json!(image);
    }
    if let Some(index) = pin.index {
        frame["index"] = json!(index);
    }
    frame
}

/// One `step` frame: the instruction, its position, and how it settled.
pub(crate) fn step_frame(image: &str, event: &StepEvent<'_>) -> Value {
    let mut frame = json!({
        "event": "step",
        "image": image,
        "index": event.index,
        "total": event.total,
        "instruction": event.instruction,
        "args": event.args,
        "instruction_width": event.instruction_width,
        "state": event.state.as_str(),
        "elapsed_ms": millis(event.elapsed),
    });
    if let Some(code) = event.exit_code {
        frame["exit_code"] = json!(code);
    }
    if let Some(phase) = event.phase {
        frame["phase"] = json!(tail_wire_label(phase));
    }
    // A pre-build external-pull row is announced to keep the build from looking frozen; it is
    // never a real planned instruction, so renderers must be able to tell the two apart.
    if event.provisional {
        frame["provisional"] = json!(true);
    }
    frame
}

/// Adds the typed facts a failure envelope needs: the instruction that ran, its source line, the
/// child exit code, exactly one `next:`, and the `-v`-only plumbing kept out of `message`.
///
/// [`FailureFacts::from_event`] derives `target`, `at`, the run id, and the runtime from this same
/// frame once; this function only merges the store-derived phase, reason, and exit code, then
/// writes the envelope back.
pub(crate) fn failure_facts(
    frame: &mut Value,
    record: Option<&RunRecord>,
    plumbing: Option<&str>,
    at: Option<Counter>,
) {
    let mut facts = FailureFacts::from_event(frame, at);
    if let Some(record) = record {
        for (key, value) in [
            ("phase", &record.phase),
            ("instruction", &record.instruction),
            ("source", &record.source),
        ] {
            if !value.is_empty() && frame.get(key).is_none() {
                frame[key] = json!(value);
            }
        }
        if facts.phase.is_none() && !record.phase.is_empty() {
            facts.phase = Some(record.phase.clone());
        }
        if facts.reason.is_empty()
            && let Some(reason) = record.reason.as_deref().filter(|reason| !reason.is_empty())
        {
            facts.reason = reason.to_owned();
        }
        if facts.exit_code.is_none() {
            facts.exit_code = record.exit_code;
        }
    }
    if frame.get("exit_code").is_some() {
        facts.exit_code = frame["exit_code"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok());
    }
    // Exactly one remedy on the wire: the message never repeats a `next:` tail.
    let message = frame["message"].as_str().map(|message| {
        message
            .lines()
            .filter(|line| !line.trim_start().starts_with("next:"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    if frame.get("target").is_none()
        && let Some(target) = &facts.target
    {
        frame["target"] = json!(target);
    }
    frame["next"] = json!(facts.next);
    if let Some(message) = message {
        frame["message"] = json!(message);
    }
    if let Some(plumbing) = plumbing {
        frame["runtime"] = json!(plumbing);
    }
    if !facts.reason.is_empty() {
        frame["reason"] = json!(facts.reason);
    }
    frame["failure"] = json!(facts);
}

/// Step wall time in whole milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Wall-clock epoch milliseconds stamped onto one live `log` frame.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(millis)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{StepPin, failure_facts, step_frame, tail_phase_from_wire, tail_wire_label};
    use slipfork_image::{StepEvent, StepPlace, StepState};
    use slipfork_world::snapshot::TailPhase;
    use std::time::Duration;

    #[test]
    fn tail_wire_labels_round_trip_over_the_typed_phases() {
        for phase in [
            TailPhase::Cloning,
            TailPhase::Executing,
            TailPhase::Sealing,
            TailPhase::Recording,
            TailPhase::Publishing,
        ] {
            let label = tail_wire_label(phase);
            assert_eq!(tail_phase_from_wire(label), Some(phase));
            assert_eq!(StepPin::tail_label(phase), label);
            assert_eq!(StepPin::tail_phase(label), Some(phase));
        }
        assert_eq!(tail_phase_from_wire("nope"), None);
    }

    #[test]
    fn tail_step_frames_carry_the_typed_phase_on_the_wire() {
        let mut event = StepEvent::new(
            StepPlace::new(6, 7, 0),
            "RUN",
            "sh /tmp/oni.sh",
            StepState::Tail,
            Duration::ZERO,
            None,
        );
        event.phase = Some(TailPhase::Sealing);
        let frame = step_frame("api", &event);
        assert_eq!(frame["state"], "tail");
        assert_eq!(frame["phase"], "sealing");

        let started = StepEvent::new(
            StepPlace::new(6, 7, 0),
            "RUN",
            "sh /tmp/oni.sh",
            StepState::Started,
            Duration::ZERO,
            None,
        );
        assert!(step_frame("api", &started).get("phase").is_none());
    }

    #[test]
    fn a_failed_frame_carries_one_exact_failure_envelope() {
        let mut frame = serde_json::json!({
            "event": "failed",
            "kind": "run",
            "name": "api",
            "run_id": "abc123",
            "exit_code": 3,
            "message": "boom\nnext: fix it",
            "elapsed_ms": 1200,
        });
        failure_facts(&mut frame, None, None, None);
        assert_eq!(
            frame["failure"],
            serde_json::json!({
                "code": "boot-failed",
                "kind": "run",
                "phase": null,
                "target": "api",
                "at": null,
                "exit_code": 3,
                "run_id": "abc123",
                "reason": "",
                "runtime_ms": 1200,
                "next": "slipfork logs -r abc123",
            })
        );
        assert_eq!(frame["message"], "boom");
        assert_eq!(frame["next"], "slipfork logs -r abc123");

        let mut plumbed = serde_json::json!({
            "event": "failed",
            "kind": "run",
            "name": "api",
            "run_id": "abc123",
        });
        failure_facts(&mut plumbed, None, Some("uv run seed --fresh"), None);
        assert_eq!(plumbed["runtime"], "uv run seed --fresh");
        let mut bare = serde_json::json!({
            "event": "failed",
            "kind": "run",
            "name": "api",
            "run_id": "abc123",
        });
        failure_facts(&mut bare, None, None, None);
        assert!(bare.get("runtime").is_none());

        let mut positioned = serde_json::json!({
            "event": "failed",
            "kind": "step",
            "name": "api",
        });
        failure_facts(
            &mut positioned,
            None,
            None,
            Some(super::Counter { index: 2, total: 3 }),
        );
        assert_eq!(
            positioned["failure"]["at"],
            serde_json::json!({ "index": 2, "total": 3 })
        );
    }
}
