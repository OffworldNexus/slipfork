//! `env`, `secret`, `open`, and `auth`: the host-facing daily verbs.
//!
//! Every ambient fact (terminal, hostname, display, opener, state directory) arrives through the
//! injected [`HostEnv`]; this module never reads the environment. Secret bytes stay inside the
//! daemon's `0600` vault unless the caller asked for `--reveal` on a real terminal.

use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;

use serde_json::{Value, json};
use slipfork_ospp::{
    FileDeviceKeyStore, LivePairingUrl, OsppClient, OsppEndpoint, OsppError, OsppTimeouts,
    PairPhase, PairingStore, SoftwareP256Signer, WsOsppClient,
};

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::render::paint;

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

/// Parsed `slipfork open` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenArgs {
    /// Open this service's route instead of the world default.
    pub service: Option<String>,
    /// Emit `{url, opened}` with zero ANSI.
    pub json: bool,
}

/// Parsed `slipfork auth` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthArgs {
    /// `--ospp-endpoint`: the highest-precedence endpoint.
    pub endpoint: Option<String>,
    /// The positional `slipfork auth <url>` endpoint.
    pub positional: Option<String>,
    /// Emit `{endpoint, account, machine}` with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Prints resolved runtime variables; secrets stay redacted unless `--reveal` on a TTY.
    pub fn env(&self, args: &EnvArgs) -> ExitCode {
        if args.reveal && !self.tty() {
            return fail(
                "`--reveal` needs an interactive terminal",
                "run `slipfork env --reveal` directly in your terminal",
            );
        }
        let mut client = match DaemonClient::connect(&self.host) {
            Ok(client) => client,
            Err(error) => return self.daemon_failure(&error, args.json),
        };
        let params = json!({ "project": self.cwd, "component": args.component });
        let reply = match client.call_streaming("env", params, &mut |_| {}) {
            Ok(reply) => reply,
            Err(error) => return self.env_failure(&error, args.json),
        };
        let Some(components) = reply.get("components").and_then(Value::as_object) else {
            return fail(
                "the daemon sent no env components",
                "run `slipfork status` and retry",
            );
        };
        let reveal = args.reveal && self.tty();
        if args.json {
            let mut out = serde_json::Map::new();
            for (component, variables) in components {
                let mut rows = serde_json::Map::new();
                for (key, info) in variables.as_object().into_iter().flatten() {
                    rows.insert(key.clone(), Value::String(display(info, reveal)));
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
                text.push_str(&format!("{}\n", paint(component, "1", self.color)));
            }
            for (key, info) in rows {
                let secret = is_secret(info);
                let value = display(info, reveal);
                let shown = if secret && !reveal && info["set"].as_bool() == Some(true) {
                    paint(&value, "2", self.color)
                } else {
                    value
                };
                text.push_str(&format!("{key:<width$}  {shown}\n"));
            }
        }
        emit(&text)
    }

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
                        let shown = if entry["set"].as_bool() == Some(true) {
                            "[REDACTED]"
                        } else {
                            "<unset>"
                        };
                        rows.insert(key.to_owned(), Value::String(shown.to_owned()));
                    }
                }
                out.insert(component, Value::Object(rows));
            }
            return emit(&format!("{}\n", Value::Object(out)));
        }
        let multiple = components.len() > 1;
        let mut text = String::new();
        for (component, keys) in components {
            let rows: Vec<&Value> = keys.as_array().into_iter().flatten().collect();
            let width = rows
                .iter()
                .filter_map(|entry| entry.get("key").and_then(Value::as_str))
                .map(str::len)
                .max()
                .unwrap_or(0);
            if multiple {
                text.push_str(&format!("{}\n", paint(&component, "1", self.color)));
            }
            for entry in rows {
                let Some(key) = entry.get("key").and_then(Value::as_str) else {
                    continue;
                };
                let value = if entry["set"].as_bool() == Some(true) {
                    paint("[REDACTED]", "2", self.color)
                } else {
                    "<unset>".to_owned()
                };
                text.push_str(&format!("{key:<width$}  {value}\n"));
            }
        }
        if text.is_empty() {
            text.push_str("No secrets declared in this Forkfile\n");
        }
        emit(&text)
    }

    fn secret_set(&self, args: &SecretArgs) -> ExitCode {
        let Some((component, key)) = split_target(args.target.as_deref()) else {
            return fail(
                "`secret set` needs `component.KEY`",
                "run `slipfork secret list` to see declared keys",
            );
        };
        let value = match read_secret_value(self.tty(), &format!("{component}.{key}")) {
            Ok(value) => value,
            Err(message) => return fail(&message, "pipe the value or run interactively"),
        };
        if value.is_empty() {
            return fail(
                "the secret value is empty",
                &format!(
                    "pipe a value: printf '%s' \"$VALUE\" | slipfork secret set {component}.{key}"
                ),
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
        emit(&format!(
            "{} Set {component}.{key}\n",
            paint("✓", "32", self.color)
        ))
    }

    fn secret_unset(&self, args: &SecretArgs) -> ExitCode {
        let Some((component, key)) = split_target(args.target.as_deref()) else {
            return fail(
                "`secret unset` needs `component.KEY`",
                "run `slipfork secret list` to see stored keys",
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
        emit(&format!(
            "{} Unset {component}.{key}\n",
            paint("✓", "32", self.color)
        ))
    }

    /// Opens the world or service URL, or prints it on a headless host.
    pub fn open(&self, args: &OpenArgs) -> ExitCode {
        let world = crate::world::world_name(&self.cwd, None)
            .map(|world| world.as_str().to_owned())
            .unwrap_or_else(|_| "slipfork".to_owned());
        let base = format!("http://{world}.localhost");
        let candidates = match route_candidates(&self.cwd, &base, args.service.as_deref()) {
            Ok(candidates) => candidates,
            Err(message) => {
                return self.report(
                    "forkfile",
                    &message,
                    "create a Forkfile in this directory, then run `slipfork up`",
                    args.json,
                );
            }
        };
        if candidates.len() > 1 {
            let mut text = String::new();
            for url in &candidates {
                text.push_str(&format!("{}\n", paint(url, "1;36", self.color)));
            }
            let _ = emit(&text);
            return self.report(
                "ambiguous-target",
                "several routes match",
                "pass a service: `slipfork open <service>`",
                args.json,
            );
        }
        let url = candidates.first().cloned().unwrap_or(base);
        let headless = self.host.ssh || !self.host.display;
        if args.json {
            return emit(&format!(
                "{}\n",
                json!({ "url": url, "opened": !headless && self.host.opener.is_some() })
            ));
        }
        if headless {
            return emit(&format!(
                "{}\nnext: open it on a machine with a browser\n",
                paint(&url, "1;36", self.color)
            ));
        }
        let Some(opener) = &self.host.opener else {
            return emit(&format!(
                "{}\nnext: install xdg-open, or open the URL in a browser\n",
                paint(&url, "1;36", self.color)
            ));
        };
        match Command::new(opener).arg(&url).stdin(Stdio::null()).status() {
            Ok(status) if status.success() => emit(&format!(
                "{} Opened {}\n",
                paint("✓", "32", self.color),
                paint(&url, "1;36", self.color)
            )),
            _ => fail(
                &format!("cannot open {url}"),
                "open the URL in a browser, then retry",
            ),
        }
    }

    /// Pairs this machine with an OSPP v1 endpoint, or reuses the settled record.
    pub fn auth(&self, args: &AuthArgs) -> ExitCode {
        let raw = match args.endpoint.clone().or_else(|| args.positional.clone()) {
            Some(raw) => raw,
            None => match PairingStore::new(&self.host.state_dir).load() {
                Ok(Some(state)) => state.endpoint,
                Ok(None) => {
                    return self.report(
                        "no-endpoint",
                        "this daemon has no OSPP endpoint",
                        "slipfork auth <url>",
                        args.json,
                    );
                }
                Err(error) => {
                    return self.report(
                        "pairing-store",
                        &format!("cannot read the pairing record: {error}"),
                        "delete `<state_dir>/ospp.json` and run `slipfork auth <url>`",
                        args.json,
                    );
                }
            },
        };
        let endpoint = match OsppEndpoint::parse(&raw) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                return self.report(
                    "bad-endpoint",
                    &format!("`{raw}` is not a valid OSPP endpoint: {error}"),
                    "pass a `wss://` URL (or `ws://` on loopback)",
                    args.json,
                );
            }
        };
        let store = FileDeviceKeyStore::new(self.host.state_dir.join("device-key.json"));
        let signer = match SoftwareP256Signer::from_store(&store) {
            Ok(signer) => signer,
            Err(error) => return self.ospp_failure(&error, args.json),
        };
        let client = WsOsppClient::new(
            endpoint.clone(),
            Arc::new(signer),
            Some(self.host.state_dir.join("ospp.json")),
            OsppTimeouts::default(),
        );
        let color = self.color();
        let json_mode = args.json;
        let mut phase = |phase: PairPhase| {
            if json_mode {
                return;
            }
            let label = match phase {
                PairPhase::SolvingAltcha => "solving ALTCHA",
                PairPhase::AwaitingApproval => "awaiting approval",
                PairPhase::Approved => "approved",
            };
            eprintln!("{} {label}", paint("→", "1;36", color));
        };
        let host = self.host.clone();
        let mut on_handle = |live: &LivePairingUrl| -> Result<(), OsppError> {
            let url = live.as_str().to_owned();
            if host.ssh || !host.display {
                if json_mode {
                    eprintln!("{url}");
                } else {
                    println!("{url}");
                }
                return Ok(());
            }
            match &host.opener {
                Some(opener)
                    if Command::new(opener)
                        .arg(&url)
                        .status()
                        .is_ok_and(|s| s.success()) =>
                {
                    Ok(())
                }
                _ => {
                    println!("{url}");
                    Ok(())
                }
            }
        };
        match client.pair(&self.host.hostname, &mut phase, &mut on_handle) {
            Ok(approval) => {
                if args.json {
                    return emit(&format!(
                        "{}\n",
                        json!({
                            "endpoint": endpoint.as_str(),
                            "account": approval.account,
                            "machine": approval.machine,
                        })
                    ));
                }
                emit(&format!(
                    "{} Paired {}@{} -> {}\nnext: slipfork up\n",
                    paint("✓", "32", color),
                    approval.account,
                    approval.machine,
                    paint(endpoint.as_str(), "1;36", color),
                ))
            }
            Err(error) => self.ospp_failure(&error, args.json),
        }
    }

    fn daemon_failure(&self, error: &DaemonError, json: bool) -> ExitCode {
        self.report(
            &error.code,
            &error.message,
            "start it with `slipfork up` and retry",
            json,
        )
    }

    fn env_failure(&self, error: &DaemonError, json: bool) -> ExitCode {
        let next = match error.code.as_str() {
            "no-forkfile" => "create a Forkfile in this directory, then run `slipfork up`",
            "invalid-forkfile" => "fix the Forkfile, then retry",
            "unknown-component" => "run `slipfork env` to list this Forkfile's components",
            "unknown-secret" => "run `slipfork secret list` to see declared keys",
            "no-secret" => "run `slipfork secret list` to see stored keys",
            "vault" => "check that the state directory is writable and not group-readable",
            _ => "run `slipfork status` to inspect the daemon",
        };
        self.report(&error.code, &error.message, next, json)
    }

    fn ospp_failure(&self, error: &OsppError, json: bool) -> ExitCode {
        let text = error.to_string();
        let (message, next) = match text.split_once("\nnext: ") {
            Some((message, next)) => (message.to_owned(), next.to_owned()),
            None => (
                text,
                "run `slipfork auth <url>` again, or check the endpoint".to_owned(),
            ),
        };
        let code = error.code().unwrap_or("ospp");
        self.report(code, &message, &next, json)
    }
}

fn fail(message: &str, next: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("next: {next}");
    ExitCode::FAILURE
}

fn is_secret(info: &Value) -> bool {
    info.get("secret").and_then(Value::as_bool).unwrap_or(false)
}

fn display(info: &Value, reveal: bool) -> String {
    let set = info.get("set").and_then(Value::as_bool).unwrap_or(false);
    if is_secret(info) && !reveal {
        return if set {
            "[REDACTED]".to_owned()
        } else {
            "<unset>".to_owned()
        };
    }
    info.get("value")
        .and_then(Value::as_str)
        .map_or_else(|| "<computed>".to_owned(), str::to_owned)
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

/// Route URLs for the world, narrowed to `service` when one was named.
fn route_candidates(
    cwd: &std::path::Path,
    base: &str,
    service: Option<&str>,
) -> Result<Vec<String>, String> {
    let path = cwd.join("Forkfile");
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let document = slipfork_forkfile::parse(&source).map_err(|diagnostics| {
        diagnostics.first().map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
        )
    })?;
    let declared = document.services();
    if let Some(name) = service
        && !declared.iter().any(|declared| declared.id == name)
    {
        return Err(format!("`{name}` is not a service in this Forkfile"));
    }
    let mut urls: Vec<String> = Vec::new();
    for declared in &declared {
        if service.is_some_and(|name| name != declared.id) {
            continue;
        }
        if declared.routes.is_empty() {
            continue;
        }
        for route in &declared.routes {
            let path = route.path.clone().unwrap_or_default();
            let url = if path.is_empty() {
                base.to_owned()
            } else {
                format!("{base}{path}")
            };
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    Ok(urls)
}
