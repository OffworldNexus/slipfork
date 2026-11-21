//! Parsing and rendering for the command line that ships in this binary.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A parsed, ready-to-render command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLine {
    verb: Verb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Help,
    Version,
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
        let mut verb = None;

        for arg in args {
            let text = arg
                .to_str()
                .ok_or_else(|| UsageError::new("arguments must be valid UTF-8"))?;
            let requested = match text {
                "-h" | "--help" | "help" => Verb::Help,
                "-V" | "--version" | "version" => Verb::Version,
                other => return Err(UsageError::new(format!("unknown argument `{other}`"))),
            };

            if verb.replace(requested).is_some() {
                return Err(UsageError::new("expected one of: help, version"));
            }
        }

        Ok(Self {
            verb: verb.unwrap_or(Verb::Help),
        })
    }

    /// Writes the requested output to stdout.
    pub fn run(self, color: bool) -> ExitCode {
        let text = self.render(color);
        let mut stdout = io::stdout().lock();

        if let Err(error) = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
        {
            eprintln!("error: failed to write output: {error}");
            eprintln!("next: check that stdout is writable");
            return ExitCode::FAILURE;
        }

        ExitCode::SUCCESS
    }

    fn render(self, color: bool) -> String {
        match self.verb {
            Verb::Help => help_text(color),
            Verb::Version => format!("slipfork {VERSION}\n"),
        }
    }
}

fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_owned()
    }
}

fn help_text(color: bool) -> String {
    let title = paint(&format!("slipfork {VERSION}"), "1;36", color);
    format!(
        "{title}\n\
         Copy-on-write Btrfs environments for polyglot apps and AI coding agents.\n\
         \n\
         Usage: slipfork <command>\n\
         \n\
         Commands:\n\
         \x20 help      Print this overview\n\
         \x20 version   Print the version\n\
         \n\
         Example:\n\
         \x20 slipfork --version\n\
         \n\
         Options:\n\
         \x20 -h, --help      Print this overview\n\
         \x20 -V, --version   Print the version\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{CommandLine, Verb};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Result<CommandLine, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        CommandLine::parse(&args).map_err(|error| error.to_string())
    }

    #[test]
    fn only_the_shipped_surface_parses() {
        assert_eq!(parse(&[]).unwrap().verb, Verb::Help);
        for args in [["--help"], ["-h"], ["help"], ["--version"], ["-V"]] {
            assert!(parse(&args).is_ok(), "{args:?} should parse");
        }
        assert_eq!(parse(&["--version"]).unwrap().verb, Verb::Version);
        assert!(
            parse(&["up"])
                .unwrap_err()
                .contains("unknown argument `up`")
        );
        assert_eq!(
            parse(&["--help", "--version"]).unwrap_err(),
            "expected one of: help, version"
        );
    }

    #[test]
    fn plain_help_has_no_escape_sequences_and_an_example() {
        let text = parse(&["--help"]).unwrap().render(false);
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(text.contains("Example:\n  slipfork --version\n"), "{text}");
        assert_eq!(
            parse(&["--version"]).unwrap().render(false),
            format!("slipfork {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
