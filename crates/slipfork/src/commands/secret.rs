//! `slipfork secret`: list, store, and remove local offline secrets through the daemon vault.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::{Session, emit};
use crate::daemon::DaemonClient;
use crate::ui::Palette;
use crate::ui::table::{Column, Table};

/// The three columns of one `secret list`: key, separator, redacted value.
const SECRET: [Column; 3] = [Column::left(), Column::left(), Column::left()];

fn table() -> Table<'static> {
    Table::new(&SECRET).gap(" ")
}

/// The one redacted cell a secret entry ever renders, human or JSON.
fn shown(entry: &Value) -> &'static str {
    if entry["set"].as_bool() == Some(true) {
        "[REDACTED]"
    } else if entry["minted"].as_bool() == Some(true) {
        "<minted on first up>"
    } else {
        "<unset>"
    }
}

/// Which `secret` subcommand to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretVerb {
    /// List declared secrets, always redacted.
    List,
    /// Store one value read from stdin.
    Set,
    /// Remove exactly one stored key.
    Unset,
}

/// Parsed `slipfork secret` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretArgs {
    /// The chosen subcommand.
    pub verb: SecretVerb,
    /// `component.KEY` for `set`/`unset`, or an optional component for `list`.
    pub target: Option<String>,
    /// Emit objects/arrays with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Lists, stores, or removes one offline secret.
    pub fn secret(&self, args: &SecretArgs) -> ExitCode {
        match args.verb {
            SecretVerb::List => self.secret_list(args),
            SecretVerb::Set => self.secret_set(args),
            SecretVerb::Unset => self.secret_unset(args),
        }
    }

    fn secret_list(&self, args: &SecretArgs) -> ExitCode {
        let mut client = match DaemonClient::connect(&self.host) {
            Ok(client) => client,
            Err(error) => return self.daemon_failure(&error, args.json),
        };
        let params = json!({ "project": self.cwd, "component": args.target });
        let reply = match client.call_streaming("secret.list", params, &mut |_| {}) {
            Ok(reply) => reply,
            Err(error) => return self.env_failure(&error, args.json),
        };
        let components = reply
            .get("components")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if args.json {
            let mut out = serde_json::Map::new();
            for (component, keys) in components {
                let mut rows = serde_json::Map::new();
                for entry in keys.as_array().into_iter().flatten() {
                    if let Some(key) = entry.get("key").and_then(Value::as_str) {
                        rows.insert(key.to_owned(), Value::String(shown(entry).to_owned()));
                    }
                }
                out.insert(component, Value::Object(rows));
            }
            return emit(&format!("{}\n", Value::Object(out)));
        }
        let palette = Palette::new(self.color());
        let multiple = components.len() > 1;
        let table = table();
        let mut text = String::new();
        for (component, keys) in components {
            let rows: Vec<&Value> = keys.as_array().into_iter().flatten().collect();
            let widths = table.widths(
                &rows
                    .iter()
                    .filter_map(|entry| {
                        let key = entry.get("key").and_then(Value::as_str)?;
                        Some(vec![
                            key.to_owned(),
                            "=".to_owned(),
                            shown(entry).to_owned(),
                        ])
                    })
                    .collect::<Vec<_>>(),
            );
            if multiple {
                text.push_str(&format!("{}\n", palette.label(&component)));
            }
            for entry in rows {
                let Some(key) = entry.get("key").and_then(Value::as_str) else {
                    continue;
                };
                let value = if entry["set"].as_bool() == Some(true) {
                    palette.redacted(shown(entry))
                } else {
                    palette.dim(shown(entry))
                };
                text.push_str(&table.line(&widths, &[palette.key(key), palette.dim("="), value]));
                text.push('\n');
            }
        }
        if text.is_empty() {
            text.push_str("No secrets declared in this Forkfile\n");
        }
        emit(&text)
    }

    fn secret_set(&self, args: &SecretArgs) -> ExitCode {
        let Some((component, key)) = split_target(args.target.as_deref()) else {
            return self.report(
                "missing-target",
                "`secret set` needs `component.KEY`",
                "run `slipfork secret list` to see declared keys",
                args.json,
            );
        };
        let value = match read_secret_value(self.interactive(), &format!("{component}.{key}")) {
            Ok(value) => value,
            Err(message) => {
                return self.report(
                    "secret-input",
                    &message,
                    "pipe the value or run interactively",
                    args.json,
                );
            }
        };
        if value.is_empty() {
            let next = format!(
                "pipe a value: printf '%s' \"$VALUE\" | slipfork secret set {component}.{key}"
            );
            return self.report(
                "empty-secret",
                "the secret value is empty",
                &next,
                args.json,
            );
        }
        let mut client = match DaemonClient::connect(&self.host) {
            Ok(client) => client,
            Err(error) => return self.daemon_failure(&error, args.json),
        };
        let params = json!({
            "project": self.cwd,
            "component": component,
            "key": key,
            "value": value,
        });
        let reply = match client.call_streaming("secret.set", params, &mut |_| {}) {
            Ok(reply) => reply,
            Err(error) => return self.env_failure(&error, args.json),
        };
        if args.json {
            return emit(&format!(
                "{}\n",
                json!({ "component": reply["component"], "key": reply["key"], "set": true })
            ));
        }
        let palette = Palette::new(self.color());
        emit(&format!(
            "{} Set {}\n",
            palette.icon_ok(),
            palette.path(&format!("{component}.{key}"))
        ))
    }

    fn secret_unset(&self, args: &SecretArgs) -> ExitCode {
        let Some((component, key)) = split_target(args.target.as_deref()) else {
            return self.report(
                "missing-target",
                "`secret unset` needs `component.KEY`",
                "run `slipfork secret list` to see stored keys",
                args.json,
            );
        };
        let mut client = match DaemonClient::connect(&self.host) {
            Ok(client) => client,
            Err(error) => return self.daemon_failure(&error, args.json),
        };
        let params = json!({ "project": self.cwd, "component": component, "key": key });
        let reply = match client.call_streaming("secret.unset", params, &mut |_| {}) {
            Ok(reply) => reply,
            Err(error) => return self.env_failure(&error, args.json),
        };
        if args.json {
            return emit(&format!(
                "{}\n",
                json!({ "component": reply["component"], "key": reply["key"], "unset": true })
            ));
        }
        let palette = Palette::new(self.color());
        emit(&format!(
            "{} Unset {}\n",
            palette.icon_ok(),
            palette.path(&format!("{component}.{key}"))
        ))
    }
}

fn split_target(target: Option<&str>) -> Option<(&str, &str)> {
    let target = target?;
    let (component, key) = target.split_once('.')?;
    if component.is_empty() || key.is_empty() {
        return None;
    }
    Some((component, key))
}

/// Reads one value from stdin; on a terminal the prompt is shown and echo is disabled.
fn read_secret_value(tty: bool, label: &str) -> Result<String, String> {
    if tty {
        eprint!("value for {label}: ");
        let _ = io::stderr().flush();
    }
    let _echo = if tty { EchoGuard::disable() } else { None };
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|error| format!("cannot read the secret value: {error}"))?;
    if tty {
        eprintln!();
    }
    Ok(buffer.trim_end_matches(['\n', '\r']).to_owned())
}

/// Restores the terminal echo flag on drop.
struct EchoGuard {
    saved: libc::termios,
}

impl EchoGuard {
    fn disable() -> Option<Self> {
        // SAFETY: both calls operate on a live `termios` for stdin and never share memory.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return None;
            }
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &quiet) != 0 {
                return None;
            }
            Some(Self { saved })
        }
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        // SAFETY: restores the exact flags captured in `disable`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::table;

    #[test]
    fn secrets_share_one_key_column_and_stay_redacted() {
        let rows = vec![
            vec![
                "DATABASE_URL".to_owned(),
                "=".to_owned(),
                "[REDACTED]".to_owned(),
            ],
            vec!["API_KEY".to_owned(), "=".to_owned(), "<unset>".to_owned()],
        ];
        let widths = table().widths(&rows);
        let lines: Vec<String> = rows.iter().map(|row| table().line(&widths, row)).collect();
        assert_eq!(
            lines,
            vec!["DATABASE_URL = [REDACTED]", "API_KEY      = <unset>"]
        );
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
        assert_eq!(lines[0].find('='), lines[1].find('='), "{lines:?}");
        assert!(!lines.join("\n").contains("hunter2"));
    }
}
