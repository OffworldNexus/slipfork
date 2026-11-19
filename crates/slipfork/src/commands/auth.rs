//! `slipfork auth`: pair this machine with the daemon's OSPP endpoint.

use std::process::ExitCode;

use serde_json::json;
use slipfork_edge::DaemonControl;

use crate::CliApp;
use crate::commands::control::connect;

/// Ask the daemon for a pairing URL and print it (JSON under `--json`).
pub(crate) fn login(app: &CliApp, endpoint: Option<String>) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let auth = client
        .auth_login(endpoint.as_deref())
        .map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "key_id": auth.key_id,
                "backend": auth.backend,
                "account": auth.account,
                "machine": auth.machine,
                "pairing_url": auth.pairing_url.expose(),
            })
        );
    } else {
        println!("{}", auth.pairing_url.expose());
        println!(
            "[auth] paired {} (backend={}){}",
            auth.key_id,
            auth.backend,
            auth.account
                .as_ref()
                .map(|account| format!(" account={account}"))
                .unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}
