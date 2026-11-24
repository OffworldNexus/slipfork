//! Typed NDJSON frames for live build rows and boot failures: shapes only, zero rendering.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_image::StepEvent;
use slipfork_world::run_log::RunRecord;

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
        "state": event.state.as_str(),
        "elapsed_ms": millis(event.elapsed),
    });
    if let Some(code) = event.exit_code {
        frame["exit_code"] = json!(code);
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
        .unwrap_or_else(|| "inspect `.slipfork/supervisor.log`".to_owned());
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
