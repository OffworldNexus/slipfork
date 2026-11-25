//! The one `--json` emitter: a world verb writes raw NDJSON here and nowhere else.

use std::process::ExitCode;

use serde_json::Value;

use crate::commands::emit;

/// Emits one streamed frame when `--json` is on: `--verbose` keeps `note` frames, else they drop.
pub(in crate::world) fn emit_stream(json: bool, verbose: bool, event: &Value) {
    if json && (verbose || event["event"].as_str() != Some("note")) {
        let _ = emit_json(event);
    }
}

/// Emits one already-built JSON object with zero ANSI.
pub(in crate::world) fn emit_json(value: &Value) -> ExitCode {
    emit(&format!("{value}\n"))
}
