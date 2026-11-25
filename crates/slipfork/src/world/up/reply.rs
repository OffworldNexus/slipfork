//! Daemon `ps` decoding plus the typed boot-node reply projection.

use serde_json::{Value, json};

use crate::daemon::{DaemonClient, DaemonError};

/// Reads the daemon's typed `ps` reply so target resolution has one canonical registry view.
pub(super) fn ps_reply(client: &mut DaemonClient) -> Result<crate::ps::PsReply, DaemonError> {
    let value = client.call_streaming("ps", json!({}), &mut |_| {})?;
    crate::status::parse(&value).map_err(|error| {
        DaemonError::new(
            "ps-decode",
            format!("cannot decode the daemon's ps reply: {error}"),
        )
    })
}

/// One completed boot node, projected into the `specs/02` `nodes` array.
///
/// Exactly one row per node: live `started` frames feed `tui.observe` for the spinner only,
/// never the published contract.
pub(super) fn boot_node(event: &Value) -> Option<Value> {
    if !matches!(
        event["event"].as_str(),
        Some("finished") | Some("up-to-date")
    ) {
        return None;
    }
    let kind = event["kind"].as_str()?;
    let name = event["name"].as_str()?;
    Some(json!({
        "kind": kind,
        "name": name,
        "state": event["state"].as_str().unwrap_or("unknown"),
        "duration_ms": event["duration_ms"],
    }))
}
