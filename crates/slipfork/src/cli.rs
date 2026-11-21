//! Parsing and rendering for the command line that ships in this binary.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands::{FmtArgs, MigrateArgs, Session, emit};
use crate::render::paint;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A parsed, ready-to-run command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    verb: Verb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Verb {
    Help(HelpTopic),
    Version,
    Fmt(FmtArgs),
    Migrate(MigrateArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Global,
    Fmt,
    Migrate,
}

/// A rejected command line, rendered as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    detail: String,
}

impl UsageError {
    fn new(detail: impl Into<String>) -> Self {
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
    /// Parses `args` (argv with the program name already removed).
    pub fn parse(args: &[OsString]) -> Result<Self, UsageError> {
        let texts: Vec<&str> = args
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                arg.to_str().ok_or_else(|| {
                    UsageError::new(format!("argument {} must be valid UTF-8", index + 1))
                })
            })
            .collect::<Result<_, _>>()?;
        let Some((verb, rest)) = texts.split_first() else {
            return Ok(Self {
                verb: Verb::Help(HelpTopic::Global),
            });
        };
        let verb = match *verb {
            "-h" | "--help" | "help" => {
                if !rest.is_empty() {
                    return Err(UsageError::new("expected one of: help, version"));
                }
                Verb::Help(HelpTopic::Global)
            }
            "-V" | "--version" | "version" => {
                if !rest.is_empty() {
                    return Err(UsageError::new("expected one of: help, version"));
                }
                Verb::Version
            }
            "fmt" => fmt_verb(rest)?,
            "migrate" => migrate_verb(rest)?,
            other => return Err(UsageError::new(format!("unknown argument `{other}`"))),
        };
        Ok(Self { verb })
    }

    /// Runs the parsed command line against the injected session.
    pub fn run(self, session: Session) -> ExitCode {
        match self.verb {
            Verb::Help(topic) => emit(&help_text(topic, session.color())),
            Verb::Version => emit(&format!("slipfork {VERSION}\n")),
            Verb::Fmt(args) => session.fmt(&args),
            Verb::Migrate(args) => session.migrate(&args),
        }
    }
}

fn fmt_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Fmt));
    }
    let mut check = false;
    let mut json = false;
    let mut path = None;
    for arg in args {
        match *arg {
            "--check" => check = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!("unknown flag `{other}` for `fmt`")));
            }
            other => {
                if path.replace(PathBuf::from(other)).is_some() {
                    return Err(UsageError::new("`fmt` takes at most one path"));
                }
            }
        }
    }
    Ok(Verb::Fmt(FmtArgs {
        check,
        json,
        path: path.unwrap_or_else(|| PathBuf::from("Forkfile")),
    }))
}

fn migrate_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Migrate));
    }
    let mut write = false;
    let mut force = false;
    let mut json = false;
    let mut path = None;
    for arg in args {
        match *arg {
            "--write" => write = true,
            "--force" => force = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `migrate`"
                )));
            }
            other => {
                if path.replace(PathBuf::from(other)).is_some() {
                    return Err(UsageError::new("`migrate` takes at most one path"));
                }
            }
        }
    }
    Ok(Verb::Migrate(MigrateArgs {
        write,
        force,
        json,
        path: path.unwrap_or_else(|| PathBuf::from("snow.yml")),
    }))
}

fn help_text(topic: HelpTopic, color: bool) -> String {
    match topic {
        HelpTopic::Global => {
            let title = paint(&format!("slipfork {VERSION}"), "1;36", color);
            format!(
                "{title}\n\
                 Format and migrate Forkfile documents for Slipfork.\n\
                 \n\
                 Usage: slipfork <command> [options]\n\
                 \n\
                 Commands:\n\
                 \x20 fmt       Format a Forkfile in place\n\
                 \x20 migrate   Convert a legacy snow.yml into a Forkfile\n\
                 \x20 help      Print this overview\n\
                 \x20 version   Print the version\n\
                 \n\
                 Examples:\n\
                 \x20 slipfork fmt --check Forkfile\n\
                 \x20 slipfork migrate --write snow.yml\n\
                 \n\
                 Options:\n\
                 \x20 -h, --help      Print this overview\n\
                 \x20 -V, --version   Print the version\n"
            )
        }
        HelpTopic::Fmt => {
            let title = paint("slipfork fmt", "1;36", color);
            format!(
                "{title}\n\
                 Format a Forkfile in place, or report with `--check`.\n\
                 \n\
                 Usage: slipfork fmt [--check] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH      Forkfile to format (default: Forkfile)\n\
                 \n\
                 Options:\n\
                 \x20 --check   Report whether PATH is formatted; exit 1 when not\n\
                 \x20 --json    Emit {{\"path\",\"formatted\",\"changed\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork fmt --check Forkfile\n"
            )
        }
        HelpTopic::Migrate => {
            let title = paint("slipfork migrate", "1;36", color);
            format!(
                "{title}\n\
                 Convert a legacy snow.yml manifest into a Forkfile.\n\
                 \n\
                 Usage: slipfork migrate [--write] [--force] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH     snow.yml manifest to convert (default: snow.yml)\n\
                 \n\
                 Options:\n\
                 \x20 --write  Save the result as Forkfile next to PATH\n\
                 \x20 --force  Overwrite an existing Forkfile\n\
                 \x20 --json   Emit {{\"path\",\"written\",\"forkfile\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork migrate --write snow.yml\n"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandLine, HelpTopic, Verb, help_text};
    use crate::commands::{FmtArgs, MigrateArgs};
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
        assert!(
            parse(&["up"])
                .unwrap_err()
                .contains("unknown argument `up`")
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
        let global = help_text(HelpTopic::Global, false);
        assert!(!global.contains('\u{1b}'), "{global}");
        assert!(global.contains("  fmt "), "{global}");
        assert!(global.contains("  migrate "), "{global}");
        assert!(
            global.contains("Examples:\n  slipfork fmt --check Forkfile\n"),
            "{global}"
        );
        assert!(!global.to_lowercase().contains("btrfs"), "{global}");
    }
}
