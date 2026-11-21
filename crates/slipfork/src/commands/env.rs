//! `slipfork env`: resolved runtime variables, redacted unless `--reveal` on a real terminal.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::{Session, emit};
use crate::daemon::DaemonClient;
use crate::ui::paint;

/// Parsed `slipfork env` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvArgs {
    /// Only this component's variables.
    pub component: Option<String>,
    /// Show real secret values; refused unless stdout is a terminal.
    pub reveal: bool,
    /// Emit `{component: {key: value}}` with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Prints resolved runtime variables; the daemon only sends values on `--reveal` at a TTY.
    pub fn env(&self, args: &EnvArgs) -> ExitCode {
        if args.reveal && !self.interactive() {
            return self.report(
                "reveal-needs-tty",
                "`--reveal` needs an interactive terminal",
                "run `slipfork env --reveal` directly in your terminal",
                args.json,
            );
        }
        // `--json` is machine data: it never carries a plaintext secret, even from a TTY.
        let reveal = args.reveal && self.interactive() && !args.json;
        let mut client = match DaemonClient::connect(&self.host) {
            Ok(client) => client,
            Err(error) => return self.daemon_failure(&error, args.json),
        };
        let params = json!({
            "project": self.cwd,
            "component": args.component,
            "reveal": reveal,
        });
        let reply = match client.call_streaming("env", params, &mut |_| {}) {
            Ok(reply) => reply,
            Err(error) => return self.env_failure(&error, args.json),
        };
        let Some(components) = reply.get("components").and_then(Value::as_object) else {
            return self.report(
                "env-reply",
                "the daemon sent no env components",
                "run `slipfork status` and retry",
                args.json,
            );
        };
        if args.json {
            let mut out = serde_json::Map::new();
            for (component, variables) in components {
                let mut rows = serde_json::Map::new();
                for (key, info) in variables.as_object().into_iter().flatten() {
                    rows.insert(key.clone(), Value::String(display(info)));
                }
                out.insert(component.clone(), Value::Object(rows));
            }
            return emit(&format!("{}\n", Value::Object(out)));
        }
        let multiple = components.len() > 1;
        let mut text = String::new();
        for (component, variables) in components {
            let rows: Vec<(&str, &Value)> = variables
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, info)| (key.as_str(), info))
                .collect();
            let width = rows.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
            if multiple {
                text.push_str(&format!("{}\n", paint(component, "1", self.color())));
            }
            for (key, info) in rows {
                let value = display(info);
                let shown = if is_secret(info) && info["set"].as_bool() == Some(true) {
                    paint(&value, "2", self.color())
                } else {
                    value
                };
                text.push_str(&format!("{key:<width$}  {shown}\n"));
            }
        }
        emit(&text)
    }
}

fn is_secret(info: &Value) -> bool {
    info.get("secret").and_then(Value::as_bool).unwrap_or(false)
}

/// The already-redacted cell: the daemon sent `null` unless the caller could see the value.
fn display(info: &Value) -> String {
    if let Some(value) = info.get("value").and_then(Value::as_str) {
        return value.to_owned();
    }
    if is_secret(info) {
        return if info.get("set").and_then(Value::as_bool).unwrap_or(false) {
            "[REDACTED]".to_owned()
        } else {
            "<unset>".to_owned()
        };
    }
    "<computed>".to_owned()
}
