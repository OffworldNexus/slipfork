//! `shell` and `exec`: one real-PTY relay plus the plain duplex frame path.
//!
//! A terminal gets the world's `/dev/pts/N` master through [`Session::relay_pty`], shared by both
//! verbs; a pipe, `--no-interactive`, or `--json` stays on typed `event` frames. The CLI never
//! builds argv: the daemon resolves targets against its live roster.

use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::pty;
use super::streams::{INTERRUPTED, Renderer, install_sigint, refusal_next};
use crate::commands::Session;
use crate::daemon::{DaemonClient, DaemonError};

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
    /// Demand a real PTY; fails closed without an interactive terminal.
    pub interactive: bool,
    /// Keep plain piped stdio even when stdin is a terminal.
    pub no_interactive: bool,
}

impl Session {
    /// `slipfork shell <target>`: a real PTY on a terminal, a plain pipe otherwise.
    pub fn shell(&self, args: &TargetArgs) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        if !args.json && self.interactive() && self.stdin_is_terminal() {
            let (cols, rows) = pty::geometry();
            let params = json!({
                "project": self.cwd(),
                "target": args.target,
                "tty": true,
                "cols": cols,
                "rows": rows,
            });
            return self.relay_pty(&mut client, "shell", params, args.json);
        }
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "tty": false,
        });
        let mut renderer = Renderer::exec(self.color(), args.json, self.clock());
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
                exit_status(code)
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.exact_fail(&error, "shell", args.json),
        }
    }

    /// `slipfork exec [service|image] -- <cmd>`: auto-PTY on a terminal, exit code forwarded.
    pub fn exec(&self, args: &ExecArgs) -> ExitCode {
        let stdin_tty = self.stdin_is_terminal();
        let has_tty = self.interactive() && stdin_tty;
        if args.interactive && !has_tty {
            return self.report(
                "no-tty",
                "`--interactive` requires an interactive terminal",
                "run with stdin and stdout on a terminal, or omit `-i` / pass `--no-interactive`",
                args.json,
            );
        }
        let use_pty = !args.json && !args.no_interactive && (args.interactive || has_tty);
        if use_pty {
            install_sigint();
            let Some(mut client) = self.existing_daemon(args.json) else {
                return ExitCode::FAILURE;
            };
            let (cols, rows) = pty::geometry();
            let params = json!({
                "project": self.cwd(),
                "target": args.target,
                "argv": args.argv,
                "tty": true,
                "cols": cols,
                "rows": rows,
                "select": "any",
            });
            return self.relay_pty(&mut client, "exec", params, args.json);
        }
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "argv": args.argv,
            "stdin": !(args.no_interactive && stdin_tty),
            "select": "any",
        });
        self.exec_like("exec", params, args.json, "exec")
    }

    /// The one PTY relay shared by `shell` and `exec`: attach, then collect the child's exit.
    fn relay_pty(
        &self,
        client: &mut DaemonClient,
        method: &str,
        params: Value,
        json: bool,
    ) -> ExitCode {
        let (id, master) = match client.call_pty(method, params) {
            Ok(received) => received,
            Err(error) => return self.exact_fail(&error, method, json),
        };
        if let Err(error) = pty::attach(client, &id, &master) {
            drop(master);
            let next = format!("retry `slipfork {method}` from a real terminal");
            return self.report("pty", &error.to_string(), &next, json);
        }
        drop(master);
        match client.finish_pty(&id, method) {
            Ok(value) => match value.get("exit").and_then(Value::as_i64) {
                Some(code) => exit_status(code),
                None => self.report(
                    "protocol",
                    "the daemon's PTY reply carried no exit status",
                    "retry the command; run `slipfork doctor` if it repeats",
                    json,
                ),
            },
            Err(error) => self.exact_fail(&error, method, json),
        }
    }

    fn exec_like(&self, method: &str, params: Value, json: bool, verb: &str) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(json) else {
            return ExitCode::FAILURE;
        };
        let mut renderer = Renderer::exec(self.color(), json, self.clock());
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
                exit_status(code)
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.stream_fail(&error, verb, json),
        }
    }

    /// Renders a `shell-refused` payload verbatim and everything else through [`Session::stream_fail`].
    pub(crate) fn exact_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        if json {
            let next = refusal_next(error, verb);
            return self.report(&error.code, &error.message, &next, true);
        }
        if error.code == "shell-refused" {
            eprintln!("{}", error.message);
            return ExitCode::FAILURE;
        }
        self.stream_fail(error, verb, false)
    }
}

fn exit_status(code: i64) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}
