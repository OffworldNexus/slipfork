//! `fmt` and `migrate`: file I/O and rendering around the `Forkfile` compiler.
//!
//! The composition root hands this session the working directory and the color
//! decision; nothing here reads the environment or probes the terminal.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::compose::Terminal;
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::{Palette, UiMode};
use crate::world::HostEnv;
use crate::{doctor, status};

// `mod auth;` is dormant past 0.1.0: OSPP pairing returns in a later release.
// mod auth;
mod docker;
mod env;
mod exec;
mod features;
mod fmtmigrate;
mod open;
mod pty;
mod runs;
mod secret;
mod streams;
mod timeparse;
// pub use auth::AuthArgs;
pub use docker::{DockerArgs, DockerVerb};
pub use env::EnvArgs;
pub use exec::{ExecArgs, TargetArgs};
pub use features::FeaturesArgs;
pub use fmtmigrate::{FmtArgs, MigrateArgs};
pub use open::OpenArgs;
pub use secret::{SecretArgs, SecretVerb};
pub use streams::LogsArgs;
pub use timeparse::{parse_since, parse_until};

/// One CLI invocation's environment, injected by the composition root.
#[derive(Debug, Clone)]
pub struct Session {
    cwd: PathBuf,
    terminal: Terminal,
    host: HostEnv,
}

impl Session {
    /// The injected working directory.
    #[must_use]
    pub(crate) fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// The host state resolved once by the composition root.
    #[must_use]
    pub(crate) fn host(&self) -> &HostEnv {
        &self.host
    }

    /// Builds a session from the injected working directory, terminal, and host state. No handler
    /// below ever re-probes stdio.
    #[must_use]
    pub fn new(cwd: PathBuf, terminal: Terminal, host: HostEnv) -> Self {
        Self {
            cwd,
            terminal,
            host,
        }
    }

    /// The output mode resolved once by the composition root.
    #[must_use]
    pub(crate) fn ui(&self) -> UiMode {
        self.terminal.mode
    }

    /// The terminal geometry and mode resolved once by the composition root.
    #[must_use]
    pub(crate) fn terminal(&self) -> Terminal {
        self.terminal
    }

    /// Whether ANSI styling is allowed for this invocation.
    #[must_use]
    pub fn color(&self) -> bool {
        self.terminal.mode.color()
    }

    /// Whether this invocation asked for the machine-readable object.
    #[must_use]
    pub fn json_requested(&self) -> bool {
        self.terminal.mode.json()
    }

    /// Switches to `--json` once clap has confirmed the flag was really typed.
    #[must_use]
    pub fn force_json(mut self) -> Self {
        self.terminal.mode = UiMode::Json;
        self
    }

    /// Whether stdout is a real terminal, independent of the colour decision.
    #[must_use]
    pub fn interactive(&self) -> bool {
        self.terminal.mode.interactive()
    }

    /// Whether stdin is a real terminal, resolved once by the composition root.
    #[must_use]
    pub fn stdin_is_terminal(&self) -> bool {
        self.terminal.stdin_tty
    }

    /// Renders every running world as the `specs/02` status tree.
    pub fn status(&self, json: bool, verbose: bool) -> ExitCode {
        self.render_status(json, verbose, false)
    }

    /// Renders only this directory's world or fork.
    pub fn ps(&self, json: bool, verbose: bool) -> ExitCode {
        self.render_status(json, verbose, true)
    }

    fn render_status(&self, json: bool, verbose: bool, scoped: bool) -> ExitCode {
        let value = match DaemonClient::connect_existing(&self.host) {
            Ok(Some(mut client)) => match client.call_streaming("ps", json!({}), &mut |_| {}) {
                Ok(value) => value,
                Err(error) => {
                    return self.report(
                        &error.code,
                        &error.message,
                        "start it with `slipfork up` and retry",
                        json,
                    );
                }
            },
            Ok(None) => json!({ "worlds": [] }),
            Err(error) => {
                return self.report(
                    &error.code,
                    &error.message,
                    "check that XDG_RUNTIME_DIR is readable and retry",
                    json,
                );
            }
        };
        let reply = match status::parse(&value) {
            Ok(reply) => reply,
            Err(error) => {
                return self.report(
                    "status-reply",
                    &format!("cannot decode the daemon's ps reply: {error}"),
                    "run `slipfork status --json` and report the payload",
                    json,
                );
            }
        };
        let other_worlds = !reply.worlds.is_empty();
        let reply = if scoped {
            // The `.forks` layout is resolved exactly once here, in the composition root: the
            // renderer only ever receives the resulting value struct.
            status::scope(&reply, &slipfork_world::ProjectScope::resolve(&self.cwd))
        } else {
            reply
        };
        if json {
            return match serde_json::to_string(&reply) {
                Ok(text) => emit(&format!("{text}\n")),
                Err(error) => self.fail(
                    &format!("cannot encode the ps reply: {error}"),
                    "run `slipfork status --json` and report the payload",
                ),
            };
        }
        if scoped && other_worlds && reply.worlds.is_empty() {
            return emit(&status::no_world_line(
                &self.cwd,
                self.host.home.as_deref(),
                self.color(),
            ));
        }
        let options = status::Options::new(self.color())
            .with_home(self.host.home.clone())
            .with_verbose(verbose);
        emit(&status::render(&reply, &options))
    }

    /// Prints the three host requirements and their remedies.
    pub fn doctor(&self, json: bool) -> ExitCode {
        doctor::run(&self.host, &self.cwd, json, self.color())
    }

    /// The one refusal for a host verb that never reached the daemon.
    fn daemon_failure(&self, error: &DaemonError, json: bool) -> ExitCode {
        self.report(
            &error.code,
            &error.message,
            "start it with `slipfork up` and retry",
            json,
        )
    }

    /// The one refusal renderer for `env`/`secret` control-plane failures.
    fn env_failure(&self, error: &DaemonError, json: bool) -> ExitCode {
        let next = match error.code.as_str() {
            "no-forkfile" => "create a Forkfile in this directory, then run `slipfork up`",
            "invalid-forkfile" => "fix the Forkfile, then retry",
            "unknown-component" => "run `slipfork env` to list this Forkfile's components",
            "unknown-secret" => "run `slipfork secret list` to see declared keys",
            "no-secret" => "run `slipfork secret list` to see stored keys",
            "vault" => "make the state directory writable",
            "feature" => crate::world::features::WorldFeatures::REMEDY,
            _ => "run `slipfork status` to inspect the daemon",
        };
        self.report(&error.code, &error.message, next, json)
    }

    fn fail(&self, message: &str, next: &str) -> ExitCode {
        let palette = Palette::new(self.color());
        // A daemon message may already carry its own `next:` line; that remedy wins, and the
        // renderer still prints exactly one. A typed `✗` verdict keeps its glyph instead of
        // gaining an `error:` prefix; generic failures keep the historical wording.
        let (body, embedded) = crate::world::split_remedy(message);
        let typed = body.starts_with('✗');
        let mut emitted = false;
        for line in body.lines() {
            let line = if emitted {
                line
            } else {
                line.strip_prefix("✗ ").unwrap_or(line)
            };
            if palette.color() {
                if emitted {
                    eprintln!("{}", palette.value(line));
                } else {
                    eprintln!("{} {}", palette.icon_bad(), palette.value(line));
                }
            } else if emitted {
                eprintln!("{line}");
            } else if typed {
                eprintln!("✗ {line}");
            } else {
                eprintln!("error: {line}");
            }
            emitted = true;
        }
        let next = embedded.as_deref().map_or(next, |remedy| remedy);
        if palette.color() {
            eprintln!("{} {}", palette.meta("next:"), palette.kind(next));
        } else {
            eprintln!("next: {next}");
        }
        ExitCode::FAILURE
    }

    /// One failure renderer: a JSON object under `--json`, `error:`/`next:` lines otherwise.
    pub(crate) fn report(&self, code: &str, message: &str, next: &str, json: bool) -> ExitCode {
        let (message, next) = message_and_next(message, next);
        if json {
            let message = message.strip_prefix("✗ ").unwrap_or(&message).to_owned();
            let _ = emit(&format!(
                "{}\n",
                json!({ "error": { "code": code, "message": message, "next": next } })
            ));
            return ExitCode::FAILURE;
        }
        self.fail(&message, &next)
    }

    /// Renders one feature diagnostic as `Forkfile:<line>: <message>`, plus its single `next:`.
    pub(crate) fn feature_failure(
        &self,
        diagnostic: &slipfork_forkfile::Diagnostic,
        json: bool,
    ) -> ExitCode {
        self.report(
            "feature",
            &format!(
                "Forkfile:{}: {}",
                diagnostic.span.line(),
                diagnostic.message
            ),
            &diagnostic.next,
            json,
        )
    }

    /// The `fmt`/`migrate` failure renderer: the same nested error object plus a `details` map.
    fn report_details(
        &self,
        code: &str,
        message: &str,
        details: Option<Value>,
        next: &str,
        json: bool,
    ) -> ExitCode {
        let (message, next) = message_and_next(message, next);
        if json {
            let mut error = json!({ "code": code, "message": message, "next": next });
            if let Some(details) = details {
                error["details"] = details;
            }
            let _ = emit(&format!("{}\n", json!({ "error": error })));
            return ExitCode::FAILURE;
        }
        self.fail(&message, &next)
    }
}

/// Splits a daemon message at its trailing `next: …` line; the message's own remedy wins.
pub(crate) fn message_and_next(message: &str, fallback: &str) -> (String, String) {
    let (body, embedded) = crate::world::split_remedy(message);
    (body, embedded.unwrap_or_else(|| fallback.to_owned()))
}

/// Writes one already-rendered chunk to stdout without panicking on a closed pipe.
pub(crate) fn emit(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: failed to write output: {error}");
            eprintln!("next: check that stdout is writable");
            ExitCode::FAILURE
        }
    }
}
