//! `slipfork env`: resolved runtime variables, redacted unless `--reveal` on a real terminal.

use std::process::ExitCode;

use serde_json::{Value, json};
use slipfork_forkfile::FeatureEdit;

use crate::commands::{Session, emit};
use crate::daemon::DaemonClient;
use crate::ui::Palette;
use crate::ui::table::{Column, Table};

/// The three columns of one `env` listing: key, separator, value.
const ENV: [Column; 3] = [Column::left(), Column::left(), Column::left()];

fn table() -> Table<'static> {
    Table::new(&ENV).gap(" ")
}

/// Parsed `slipfork env` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvArgs {
    /// Only this component's variables.
    pub component: Option<String>,
    /// Show real secret values; refused unless stdout is a terminal.
    pub reveal: bool,
    /// Emit `{component: {key: value}}` with zero ANSI.
    pub json: bool,
    /// Feature overrides typed on this invocation; never persisted by `env`.
    pub features: Vec<FeatureEdit>,
    /// Evaluate from an empty feature base instead of the persisted configuration.
    pub no_default_features: bool,
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
        // An `env` override is evaluated non-destructively: the daemon never writes the file.
        let params = if args.features.is_empty() && !args.no_default_features {
            params
        } else {
            json!({
                "project": self.cwd,
                "component": args.component,
                "reveal": reveal,
                "features": args.features,
                "no_default_features": args.no_default_features,
            })
        };
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
        let palette = Palette::new(self.color());
        let multiple = components.len() > 1;
        let table = table();
        let mut text = String::new();
        for (component, variables) in components {
            let rows: Vec<(&str, &Value)> = variables
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, info)| (key.as_str(), info))
                .collect();
            let widths = table.widths(
                &rows
                    .iter()
                    .map(|(key, info)| vec![(*key).to_owned(), "=".to_owned(), display(info)])
                    .collect::<Vec<_>>(),
            );
            if multiple {
                text.push_str(&format!("{}\n", palette.label(component)));
            }
            for (key, info) in rows {
                let value = display(info);
                let shown = if is_secret(info) && info["set"].as_bool() == Some(true) {
                    palette.redacted(&value)
                } else if value == "<unset>" {
                    palette.meta(&value)
                } else {
                    palette.value(&value)
                };
                text.push_str(&table.line(&widths, &[palette.key(key), palette.meta("="), shown]));
                text.push('\n');
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

#[cfg(test)]
mod tests {
    use super::table;

    #[test]
    fn env_key_values_share_one_key_column() {
        let rows = vec![
            vec![
                "API_URL".to_owned(),
                "=".to_owned(),
                "http://api.localhost".to_owned(),
            ],
            vec!["PORT".to_owned(), "=".to_owned(), "8000".to_owned()],
        ];
        let widths = table().widths(&rows);
        let lines: Vec<String> = rows.iter().map(|row| table().line(&widths, row)).collect();
        assert_eq!(
            lines,
            vec!["API_URL = http://api.localhost", "PORT    = 8000"]
        );
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
        assert_eq!(
            lines[0].find('='),
            lines[1].find('='),
            "one separator column: {lines:?}"
        );
    }
}
