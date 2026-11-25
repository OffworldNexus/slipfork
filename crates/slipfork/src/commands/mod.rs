//! `fmt` and `migrate`: file I/O and rendering around the `Forkfile` compiler.
//!
//! The composition root hands this session the working directory and the color
//! decision; nothing here reads the environment or probes the terminal.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;

use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::report::{Failure, Report, ReportRenderer, StreamKind, WallZone};
use crate::ui::{Terminal, UiMode};
use crate::world::HostEnv;
use crate::{doctor, status};

pub(crate) use crate::ui::report::emit_stdout as emit;

mod docker;
mod env;
mod exec;
mod features;
mod fmtmigrate;
mod open;
mod pty;
mod secret;
mod streams;
mod timeparse;
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
///
/// The wall zone is resolved exactly once here and shared by every human surface through
/// [`Self::renderer`]; no handler below reads `TZ` or re-probes stdio.
#[derive(Debug, Clone)]
pub struct Session {
    cwd: PathBuf,
    terminal: Terminal,
    host: HostEnv,
    wall: WallZone,
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

    /// Builds a session from the injected working directory, terminal, host state, and wall clock.
    /// No handler below ever re-probes stdio or reads the environment.
    #[must_use]
    pub fn new(cwd: PathBuf, terminal: Terminal, host: HostEnv, wall: WallZone) -> Self {
        Self {
            cwd,
            terminal,
            host,
            wall,
        }
    }

    /// The one report renderer for this invocation.
    #[must_use]
    pub(crate) fn renderer(&self) -> ReportRenderer {
        ReportRenderer::new(self.terminal, self.wall.clone())
    }

    /// The one daemon connect sink: the ephemeral hint prints here exactly once.
    pub(crate) fn connect(&self) -> Result<DaemonClient, DaemonError> {
        let renderer = self.renderer();
        DaemonClient::connect(self.host(), &mut |notice| {
            let _ = renderer.emit_notice(StreamKind::Stderr, &notice.to_string());
        })
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
        let reply = match DaemonClient::connect_existing(&self.host) {
            Ok(Some(mut client)) => match client.call_streaming("ps", json!({}), &mut |_| {}) {
                Ok(value) => match status::parse(&value) {
                    Ok(reply) => reply,
                    Err(error) => {
                        return self.report(
                            "status-reply",
                            &format!("cannot decode the daemon's ps reply: {error}"),
                            "run `slipfork status --json` and report the payload",
                            json,
                        );
                    }
                },
                Err(error) => {
                    return self.report(
                        &error.code,
                        &error.message,
                        "start it with `slipfork up` and retry",
                        json,
                    );
                }
            },
            // No daemon answers: read `state.db` read-only and never converge or write.
            Ok(None) => match status::offline(&self.host.state_dir) {
                Ok(reply) => reply,
                Err(error) => {
                    return self.report(
                        "status-ledger",
                        &format!(
                            "cannot read {}/state.db: {error}",
                            self.host.state_dir.display()
                        ),
                        "run `slipfork doctor` and check the state directory",
                        json,
                    );
                }
            },
            Err(error) => {
                return self.report(
                    &error.code,
                    &error.message,
                    "check that XDG_RUNTIME_DIR is readable and retry",
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
                Err(error) => self.report(
                    "ps",
                    &format!("cannot encode the ps reply: {error}"),
                    "run `slipfork status --json` and report the payload",
                    false,
                ),
            };
        }
        if scoped && other_worlds && reply.worlds.is_empty() {
            let text = status::no_world_line(&self.cwd, self.host.home.as_deref(), self.color());
            return self.renderer().emit_text_exit(StreamKind::Stdout, &text);
        }
        let options = status::Options::new(self.color())
            .with_home(self.host.home.clone())
            .with_verbose(verbose);
        let text = status::render(&reply, &options);
        self.renderer().emit_text_exit(StreamKind::Stdout, &text)
    }

    /// Prints the three host requirements and their remedies.
    pub fn doctor(&self, json: bool) -> ExitCode {
        doctor::run(&self.host, &self.cwd, json, &self.renderer())
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

    /// One failure renderer: a JSON object under `--json`, `error:`/`next:` lines otherwise.
    pub(crate) fn report(&self, code: &str, message: &str, next: &str, json: bool) -> ExitCode {
        let (message, next) = message_and_next(message, next);
        let failure = Failure::new(code, message, next);
        if json {
            let _ = self.renderer().json_exit(&failure.json());
        } else {
            let _ = self.renderer().emit(&Report::failure(failure));
        }
        ExitCode::FAILURE
    }

    /// Renders one typed failure through the report layer, in either mode.
    pub(crate) fn failure(&self, failure: Failure) -> ExitCode {
        let _ = self.renderer().emit(&Report::failure(failure));
        ExitCode::FAILURE
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
}

/// Splits a daemon message at its trailing `next: …` line; the message's own remedy wins.
pub(crate) fn message_and_next(message: &str, fallback: &str) -> (String, String) {
    let (body, embedded) = crate::ui::report::split_remedy(message);
    (body, embedded.unwrap_or_else(|| fallback.to_owned()))
}
