//! Typed NDJSON frames for live build rows and boot failures: shapes only, zero rendering.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_image::StepEvent;
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
    frame
}

/// Adds the typed facts a failure envelope needs: the instruction that ran, its source line, the
/// child exit code, exactly one `next:`, and the `-v`-only plumbing kept out of `message`.
pub(crate) fn failure_facts(frame: &mut Value, record: Option<&RunRecord>, plumbing: Option<&str>) {
    if frame.get("target").is_none()
        && let Some(name) = frame["name"].as_str()
    {
        frame["target"] = json!(name);
    }
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
        if frame.get("exit_code").is_none()
            && let Some(code) = record.exit_code
        {
            frame["exit_code"] = json!(code);
        }
    }
    // Exactly one remedy on the wire: the replay command when a capture exists, else the typed one;
    // the message never repeats a `next:` tail.
    let next = frame["run_id"]
        .as_str()
        .map(|id| format!("slipfork logs -r {id}"))
        .or_else(|| frame["remedy"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "run `slipfork logs` for the failing step".to_owned());
    frame["next"] = json!(next);
    if let Some(message) = frame["message"].as_str() {
        let body = message
            .lines()
            .filter(|line| !line.trim_start().starts_with("next:"))
            .collect::<Vec<_>>()
            .join("\n");
        frame["message"] = json!(body);
    }
    if let Some(plumbing) = plumbing {
        frame["runtime"] = json!(plumbing);
    }
    if let Some(reason) = record.and_then(|record| record.reason.as_deref())
        && !reason.is_empty()
    {
        frame["reason"] = json!(reason);
    }
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
    use super::{StepPin, step_frame, tail_phase_from_wire, tail_wire_label};
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
}
