//! `logs`, `shell`, and `exec`: streaming verbs rendered from typed events.
//!
//! The daemon sends `{"kind","pod","stream","line"}` events plus one `roster` event that fixes the
//! gutter width. This renderer owns dividers, `--json`, and the final `{"exit":n}`. A terminal
//! `shell` receives the world's real `/dev/pts/N` master and relays raw bytes untouched; a pipe
//! relays stdin as base64 input frames, so `psql`, `redis-cli`, and `sh` work over a plain pipe.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::paint;

/// Set by the SIGINT handler so a followed stream exits `0` on Ctrl-C.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

const PALETTE: [&str; 8] = ["36", "35", "33", "32", "34", "96", "95", "31"];

/// Parsed `slipfork logs` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsArgs {
    /// Keep the stream attached (`-f`).
    pub follow: bool,
    /// Include backing-resource pods (`-a` / `--all`).
    pub all: bool,
    /// Explicit service or resource id, or application services when `None`.
    pub service: Option<String>,
    /// Emit one JSON object per line plus a final `{"exit":0}`.
    pub json: bool,
}

/// Parsed `shell` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetArgs {
    /// Required service, image, postgres, database, or redis id.
    pub target: String,
    /// Emit events and a final `{"exit":n}` instead of human output.
    pub json: bool,
}

/// Parsed `slipfork exec` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecArgs {
    /// Explicit service/image id, when given.
    pub target: Option<String>,
    /// Emit events and a final `{"exit":n}` instead of human output.
    pub json: bool,
    /// Command and arguments after `--`.
    pub argv: Vec<String>,
}

extern "C" fn on_sigint(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn install_sigint() {
    // SAFETY: the handler only stores into an atomic. `signal` installs it (with `SA_RESTART`
    // under glibc); the follow-up `sigaction` clears that flag so a blocking read returns `EINTR`.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
        let mut action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut action) == 0 {
            action.sa_flags &= !libc::SA_RESTART;
            let _ = libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        }
    }
}

impl Session {
    /// `slipfork logs [-f] [-a] [service]`.
    pub fn logs(&self, args: &LogsArgs) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let params = json!({
            "project": self.cwd(),
            "follow": args.follow,
            "all": args.all,
            "service": args.service,
        });
        let mut renderer = Renderer::new(self.color(), args.json);
        match client.call_streaming("logs", params, &mut |event| renderer.event(event)) {
            Ok(_) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(error) => self.exact_fail(&error, "logs", args.json),
        }
    }

    /// `slipfork shell <target>`: a real PTY on a terminal, a plain pipe otherwise.
    ///
    /// The daemon owns kind dispatch: the CLI sends the target id and the daemon builds argv from
    /// its live roster. A terminal receives the `/dev/pts/N` master and raw bytes pass straight
    /// through; a pipe relays `exec` stdin as base64 frames.
    pub fn shell(&self, args: &TargetArgs) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let tty = !args.json && self.interactive() && std::io::stdin().is_terminal();
        if tty {
            let (cols, rows) = crate::commands::pty::geometry();
            let params = json!({
                "project": self.cwd(),
                "target": args.target,
                "tty": true,
                "cols": cols,
                "rows": rows,
            });
            let (id, master) = match client.call_pty("shell", params) {
                Ok(received) => received,
                Err(error) => return self.shell_fail(&error, args.json),
            };
            if let Err(error) = crate::commands::pty::attach(&mut client, &id, &master) {
                drop(master);
                return self.report(
                    "pty",
                    &error.to_string(),
                    "retry `slipfork shell` from a real terminal",
                    args.json,
                );
            }
            drop(master);
            return match client.finish_pty(&id, "shell") {
                Ok(value) => {
                    let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
                }
                Err(error) => self.shell_fail(&error, args.json),
            };
        }
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "tty": false,
        });
        let mut renderer = Renderer::exec(self.color(), args.json);
        let stdin = std::io::stdin();
        let input_fd = stdin.as_raw_fd();
        let mut input = stdin.lock();
        let outcome = client.call_duplex("shell", params, &mut input, input_fd, &mut |event| {
            renderer.event(event)
        });
        match outcome {
            Ok(value) => {
                let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                renderer.finish(code);
                ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.shell_fail(&error, args.json),
        }
    }

    /// Renders a daemon shell refusal; a kind refusal is already the exact `1 line + next:` copy.
    fn shell_fail(&self, error: &DaemonError, json: bool) -> ExitCode {
        self.exact_fail(error, "shell", json)
    }

    /// Renders a `shell-refused` payload verbatim and everything else through [`Self::stream_fail`].
    fn exact_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        if json {
            let _ = emit(&format!(
                "{}\n",
                json!({ "error": { "code": error.code, "message": error.message } })
            ));
            return ExitCode::FAILURE;
        }
        if error.code == "shell-refused" {
            eprintln!("{}", error.message);
            return ExitCode::FAILURE;
        }
        self.stream_fail(error, verb, false)
    }

    /// `slipfork exec [service|image] -- <cmd>`: forwards the child's exit code and stdin.
    pub fn exec(&self, args: &ExecArgs) -> ExitCode {
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "argv": args.argv,
            "stdin": true,
            "select": "any",
        });
        self.exec_like("exec", params, args.json, "exec")
    }

    fn exec_like(&self, method: &str, params: Value, json: bool, verb: &str) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(json) else {
            return ExitCode::FAILURE;
        };
        let mut renderer = Renderer::exec(self.color(), json);
        let stdin = std::io::stdin();
        let input_fd = stdin.as_raw_fd();
        let mut input = stdin.lock();
        let outcome = client.call_duplex(method, params, &mut input, input_fd, &mut |event| {
            renderer.event(event)
        });
        match outcome {
            Ok(value) => {
                let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                renderer.finish(code);
                ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.stream_fail(&error, verb, json),
        }
    }

    fn existing_daemon(&self, json: bool) -> Option<DaemonClient> {
        match DaemonClient::connect_existing(self.host()) {
            Ok(Some(client)) => Some(client),
            Ok(None) => {
                let _ = self.report(
                    "no-world",
                    "no world is running in this directory",
                    "run `slipfork up` first",
                    json,
                );
                None
            }
            Err(error) => {
                let _ = self.report(
                    "daemon",
                    &error.to_string(),
                    "check that XDG_RUNTIME_DIR is readable and retry",
                    json,
                );
                None
            }
        }
    }

    fn stream_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        let next = match error.code.as_str() {
            "ambiguous-target" => {
                format!("pass one of the ids above, e.g. `slipfork {verb} <id>`")
            }
            "no-target" => "run `slipfork up` or pass a service".to_owned(),
            "no-world" => "run `slipfork up` first".to_owned(),
            "world-booting" => "wait for `slipfork up` to finish, then retry".to_owned(),
            _ => "run `slipfork status` to inspect the world".to_owned(),
        };
        self.report(&error.code, &error.message, &next, json)
    }
}

/// Renders typed daemon events as gutters, dividers, or NDJSON.
struct Renderer {
    color: bool,
    json: bool,
    /// `logs` prefixes every line with its pod gutter; `exec` stays verbatim like `docker exec`.
    gutter: bool,
    pods: BTreeMap<String, &'static str>,
    width: usize,
}

impl Renderer {
    fn new(color: bool, json: bool) -> Self {
        Self::with_gutter(color, json, true)
    }

    /// The `exec`/`shell` renderer: the command's own stdout and stderr, byte for byte.
    fn exec(color: bool, json: bool) -> Self {
        Self::with_gutter(color, json, false)
    }

    fn with_gutter(color: bool, json: bool, gutter: bool) -> Self {
        Self {
            color,
            json,
            gutter,
            pods: BTreeMap::new(),
            width: 0,
        }
    }

    fn event(&mut self, event: &Value) {
        let pod = event.get("pod").and_then(Value::as_str).unwrap_or_default();
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("line");
        let stream = event
            .get("stream")
            .and_then(Value::as_str)
            .unwrap_or("stdout");
        let line = event
            .get("line")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "roster" {
            self.roster(event);
            return;
        }
        if self.json {
            let _ = emit(&format!(
                "{}\n",
                json!({"pod": pod, "stream": stream, "line": line, "kind": kind})
            ));
            return;
        }
        match kind {
            "eof" => {}
            "divider" => {
                let _ = emit(&format!("{}\n", paint(line, "2", self.color)));
            }
            _ if !self.gutter => {
                let _ = emit(&format!("{line}\n"));
            }
            _ => {
                let code = self.color_for(pod);
                let pad = " ".repeat(self.width.saturating_sub(pod.chars().count()));
                let gutter = paint(pod, code, self.color);
                let _ = emit(&format!("{gutter}{pad} │ {line}\n"));
            }
        }
    }

    /// Fixes the gutter width from the daemon's pod roster; it never grows mid-stream.
    fn roster(&mut self, event: &Value) {
        if self.json {
            return;
        }
        let pods: Vec<&str> = event
            .get("pods")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        self.width = pods
            .iter()
            .map(|pod| pod.chars().count())
            .max()
            .unwrap_or(0);
        for pod in pods {
            let _ = self.color_for(pod);
        }
    }

    fn finish(&self, code: i64) {
        if self.json {
            let _ = emit(&format!("{}\n", json!({ "exit": code })));
        }
    }

    fn color_for(&mut self, pod: &str) -> &'static str {
        let index = self.pods.len();
        self.pods
            .entry(pod.to_owned())
            .or_insert(PALETTE[index % PALETTE.len()])
    }
}
