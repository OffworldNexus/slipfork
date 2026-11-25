//! Daemon frame decoding: one NDJSON reply, and the refusal frame sent before a descriptor.

use serde_json::Value;

use crate::daemon::wire;

use super::DaemonError;

/// Parses one daemon frame; `Ok(Some(value))` is the final reply, `Ok(None)` an event.
pub(super) fn parse_response(
    line: &str,
    id: &str,
    on_event: &mut dyn FnMut(&Value),
) -> Result<Option<Value>, DaemonError> {
    let response: wire::Response = serde_json::from_str(line.trim()).map_err(|error| {
        DaemonError::new("protocol", format!("malformed daemon frame: {error}"))
    })?;
    if response.v != wire::VERSION {
        return Err(DaemonError::new(
            "protocol",
            format!(
                "the daemon speaks protocol v{}; this client speaks v{}",
                response.v,
                wire::VERSION
            ),
        ));
    }
    if response.id != id {
        return Err(DaemonError::new(
            "protocol",
            format!("unexpected daemon reply id `{}` for `{id}`", response.id),
        ));
    }
    if let Some(event) = response.event {
        on_event(&event);
        return Ok(None);
    }
    if let Some(error) = response.error {
        return Err(DaemonError {
            code: error.code,
            message: error.message,
        });
    }
    Ok(Some(response.ok.unwrap_or(Value::Null)))
}

/// Decodes a pre-descriptor daemon refusal frame into its exact typed error.
///
/// A PTY refusal (`shell-refused` for `s3`/unknown targets, `world-booting`, …) arrives as a plain
/// NDJSON `{"id":…,"error":{code,message}}` frame instead of the `pty` + `SCM_RIGHTS` payload.
pub(super) fn refusal_frame(payload: &[u8]) -> Option<DaemonError> {
    let response: wire::Response = serde_json::from_slice(payload).ok()?;
    let failure = response.error?;
    Some(DaemonError::new(&failure.code, failure.message))
}
