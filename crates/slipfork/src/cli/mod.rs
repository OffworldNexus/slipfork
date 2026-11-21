//! Parsing and rendering for the command line that ships in this binary.

mod help;
mod parse;

use std::fmt;
use std::process::ExitCode;

use crate::commands::{FmtArgs, MigrateArgs, Session, emit};
use crate::world::{DownArgs, RestartArgs, SuperviseArgs, UpArgs};

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
    Up(UpArgs),
    Down(DownArgs),
    Restart(RestartArgs),
    Fmt(FmtArgs),
    Migrate(MigrateArgs),
    Lsp,
    Supervise(SuperviseArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Global,
    Up,
    Down,
    Restart,
    Fmt,
    Migrate,
    Lsp,
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
            Verb::Up(args) => session.up(&args),
            Verb::Down(args) => session.down(&args),
            Verb::Restart(args) => session.restart(&args),
            Verb::Fmt(args) => session.fmt(&args),
            Verb::Migrate(args) => session.migrate(&args),
            Verb::Lsp => session.lsp(),
            Verb::Supervise(args) => session.supervise(&args),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::help::help_text;
    use super::{CommandLine, HelpTopic, Verb};
    use crate::commands::{FmtArgs, MigrateArgs};
    use crate::world::{DownArgs, RestartArgs, UpArgs};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<CommandLine, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        CommandLine::parse(&args).map_err(|error| error.to_string())
    }

    #[test]
    fn the_shipped_surface_parses() {
        assert_eq!(parse(&[]).unwrap().verb, Verb::Help(HelpTopic::Global));
        assert_eq!(parse(&["--version"]).unwrap().verb, Verb::Version);
        assert_eq!(
            parse(&["up"]).unwrap().verb,
            Verb::Up(UpArgs {
                json: false,
                world: None,
            })
        );
        assert_eq!(
            parse(&["up", "--world", "demo", "--json"]).unwrap().verb,
            Verb::Up(UpArgs {
                json: true,
                world: Some("demo".to_owned()),
            })
        );
        assert_eq!(
            parse(&["down", "--json"]).unwrap().verb,
            Verb::Down(DownArgs { json: true })
        );
        assert_eq!(
            parse(&["restart", "worker"]).unwrap().verb,
            Verb::Restart(RestartArgs {
                service: Some("worker".to_owned()),
                json: false,
            })
        );
        assert_eq!(
            parse(&["fmt", "--check", "custom.Forkfile"]).unwrap().verb,
            Verb::Fmt(FmtArgs {
                check: true,
                json: false,
                path: PathBuf::from("custom.Forkfile"),
            })
        );
        assert_eq!(
            parse(&["migrate", "--write", "--force", "--json"])
                .unwrap()
                .verb,
            Verb::Migrate(MigrateArgs {
                write: true,
                force: true,
                json: true,
                path: PathBuf::from("snow.yml"),
            })
        );
        assert_eq!(parse(&["lsp"]).unwrap().verb, Verb::Lsp);
        assert_eq!(
            parse(&["lsp", "--help"]).unwrap().verb,
            Verb::Help(HelpTopic::Lsp)
        );
        assert!(
            parse(&["lsp", "--stdio"])
                .unwrap_err()
                .contains("unexpected argument `--stdio` for `lsp`")
        );
        assert!(
            parse(&["frobnicate"])
                .unwrap_err()
                .contains("unknown argument `frobnicate`")
        );
        assert_eq!(
            parse(&["--help", "--version"]).unwrap_err(),
            "expected one of: help, version"
        );
        assert_eq!(
            parse(&["fmt", "--nope"]).unwrap_err(),
            "unknown flag `--nope` for `fmt`"
        );
        assert_eq!(
            parse(&["fmt", "a", "b"]).unwrap_err(),
            "`fmt` takes at most one path"
        );
    }

    #[test]
    fn per_verb_help_documents_every_flag() {
        assert_eq!(
            parse(&["fmt", "--help"]).unwrap().verb,
            Verb::Help(HelpTopic::Fmt)
        );
        assert_eq!(
            parse(&["migrate", "-h"]).unwrap().verb,
            Verb::Help(HelpTopic::Migrate)
        );
        assert_eq!(
            parse(&["up", "--help"]).unwrap().verb,
            Verb::Help(HelpTopic::Up)
        );
        let up = help_text(HelpTopic::Up, false);
        for flag in ["--world", "--json", "-h, --help"] {
            assert!(up.contains(flag), "{up}");
        }
        assert!(up.contains("slipfork up"), "{up}");
        let down = help_text(HelpTopic::Down, false);
        assert!(down.contains("slipfork down"), "{down}");
        let restart = help_text(HelpTopic::Restart, false);
        assert!(restart.contains("SERVICE"), "{restart}");
        let fmt = help_text(HelpTopic::Fmt, false);
        for flag in ["--check", "--json", "-h, --help"] {
            assert!(fmt.contains(flag), "{fmt}");
        }
        assert!(fmt.contains("slipfork fmt --check Forkfile"), "{fmt}");
        let migrate = help_text(HelpTopic::Migrate, false);
        for flag in ["--write", "--force", "--json", "-h, --help"] {
            assert!(migrate.contains(flag), "{migrate}");
        }
        assert!(
            migrate.contains("slipfork migrate --write snow.yml"),
            "{migrate}"
        );
        let lsp = help_text(HelpTopic::Lsp, false);
        assert!(lsp.contains("Usage: slipfork lsp"), "{lsp}");
        assert!(lsp.contains("format-on-save"), "{lsp}");
        let global = help_text(HelpTopic::Global, false);
        assert!(!global.contains('\u{1b}'), "{global}");
        for verb in [
            "  up ",
            "  down ",
            "  restart ",
            "  fmt ",
            "  migrate ",
            "  lsp ",
        ] {
            assert!(global.contains(verb), "missing {verb}: {global}");
        }
        assert!(global.contains("Examples:\n  slipfork up\n"), "{global}");
    }
}
