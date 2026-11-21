//! Parsing and rendering for the command line that ships in this binary.

mod help;
mod parse;

use std::fmt;
use std::process::ExitCode;

use crate::commands::{
    AuthArgs, EnvArgs, ExecArgs, FmtArgs, LogsArgs, MigrateArgs, OpenArgs, SecretArgs, Session,
    TargetArgs, emit,
};
use crate::surface::Shell;
use crate::world::{DownArgs, DropArgs, ForkArgs, RestartArgs, SuperviseArgs, UpArgs};

pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A parsed, ready-to-run command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    verb: Verb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Verb {
    Help(HelpTopic),
    Version,
    Status(StatusArgs),
    Ps(StatusArgs),
    Doctor(StatusArgs),
    DaemonInstall(DaemonInstallArgs),
    Up(UpArgs),
    Down(DownArgs),
    Restart(RestartArgs),
    Fork(ForkArgs),
    Drop(DropArgs),
    Logs(LogsArgs),
    Shell(TargetArgs),
    Exec(ExecArgs),
    Db(TargetArgs),
    Redis(TargetArgs),
    Env(EnvArgs),
    Secret(SecretArgs),
    Open(OpenArgs),
    Auth(AuthArgs),
    Fmt(FmtArgs),
    Migrate(MigrateArgs),
    Completions(Shell),
    Supervise(SuperviseArgs),
}

/// Parsed `slipfork status`, `ps`, and `doctor` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusArgs {
    /// Emit the raw object with zero ANSI.
    pub json: bool,
    /// Add supervisor lineage.
    pub verbose: bool,
}

/// Parsed `slipfork daemon install` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonInstallArgs {
    /// Install the system `:80` socket and AppArmor profile (needs root).
    pub ingress: bool,
    /// Emit `{"installed":[…],"ingress":bool}` with zero ANSI.
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Global,
    Status,
    Ps,
    Doctor,
    DaemonInstall,
    Up,
    Down,
    Restart,
    Fork,
    Drop,
    Logs,
    Shell,
    Exec,
    Db,
    Redis,
    Env,
    Secret,
    Open,
    Auth,
    Fmt,
    Migrate,
    Completions,
}

/// A rejected command line, rendered as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageError {
    detail: String,
}

impl UsageError {
    pub(super) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl CommandLine {
    /// Runs the parsed command line against the injected session.
    pub fn run(self, session: Session) -> ExitCode {
        match self.verb {
            Verb::Help(topic) => emit(&help::help_text(topic, session.color())),
            Verb::Version => emit(&format!("slipfork {VERSION}\n")),
            Verb::Status(args) => session.status(args.json, args.verbose),
            Verb::Ps(args) => session.ps(args.json, args.verbose),
            Verb::Doctor(args) => session.doctor(args.json),
            Verb::DaemonInstall(args) => session.daemon_install(args.ingress, args.json),
            Verb::Up(args) => session.up(&args),
            Verb::Down(args) => session.down(&args),
            Verb::Restart(args) => session.restart(&args),
            Verb::Fork(args) => session.fork(&args),
            Verb::Drop(args) => session.drop(&args),
            Verb::Logs(args) => session.logs(&args),
            Verb::Shell(args) => session.shell(&args),
            Verb::Exec(args) => session.exec(&args),
            Verb::Db(args) => session.db(&args),
            Verb::Redis(args) => session.redis(&args),
            Verb::Env(args) => session.env(&args),
            Verb::Secret(args) => session.secret(&args),
            Verb::Open(args) => session.open(&args),
            Verb::Auth(args) => session.auth(&args),
            Verb::Fmt(args) => session.fmt(&args),
            Verb::Migrate(args) => session.migrate(&args),
            Verb::Completions(shell) => emit(&shell.script()),
            Verb::Supervise(args) => session.supervise(&args),
        }
    }
}
