//! `slipfork secret`: the project's `0600` vault through the daemon, never printing values.

use std::io::{self, Read};
use std::process::ExitCode;

use serde_json::json;
use slipfork::cli::SecretAction;
use slipfork_edge::DaemonControl;

use crate::CliApp;
use crate::commands::control::connect;
use crate::commands::render_json;

/// Store, remove, or list the project's secret keys.
pub(crate) fn secret(app: &CliApp, action: SecretAction) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let cwd = app.ui.cwd()?;
    match action {
        SecretAction::Set { key, force } => {
            let value = read_secret(app.ui.stdin_tty)?;
            client
                .secret_set(&cwd, &key, &value, force)
                .map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", json!({ "ok": true, "key": key }));
            } else {
                println!("[secret] stored secrets::{key} (value never printed)");
            }
        }
        SecretAction::Unset { key } => {
            let removed = client
                .secret_unset(&cwd, &key)
                .map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", json!({ "ok": true, "key": key, "removed": removed }));
            } else if removed {
                println!("[secret] removed secrets::{key}");
            } else {
                println!("[secret] secrets::{key} was not stored; nothing to remove");
            }
        }
        SecretAction::List => {
            let keys = client.secret_list(&cwd).map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", render_json(&keys)?);
            } else {
                for key in keys {
                    println!("{key}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Read a secret from a pipe, refusing an interactive terminal where it would be echoed.
fn read_secret(stdin_tty: bool) -> Result<String, String> {
    if stdin_tty {
        return Err(
            "refusing to read a secret from an interactive terminal (it would be echoed); \
             pipe the value instead: printf '%s' \"$VALUE\" | slipfork secret set KEY"
                .to_owned(),
        );
    }
    let mut stdin = io::stdin();
    let mut value = String::new();
    stdin
        .read_to_string(&mut value)
        .map_err(|err| format!("cannot read the secret from stdin: {err}"))?;
    while value.ends_with('\n') || value.ends_with('\r') {
        value.pop();
    }
    if value.is_empty() {
        return Err("the secret read from stdin is empty".to_owned());
    }
    Ok(value)
}
