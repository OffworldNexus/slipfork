//! Maps the parsed `clap` tree onto the injected [`Session`], and renders usage errors.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{ColorChoice, CommandFactory, FromArgMatches};
use serde_json::json;

use crate::commands::{
    EnvArgs, ExecArgs, FeaturesArgs, FmtArgs, LogsArgs, MigrateArgs, OpenArgs, SecretArgs,
    SecretVerb, Session, TargetArgs, emit,
};
use crate::world::{DownArgs, DropArgs, ForkArgs, RestartArgs, SuperviseArgs, UpArgs};

use super::verbs::{Cli, Command, SecretCommand};

/// The one remedy every JSON usage rejection carries.
const USAGE_NEXT: &str = "run `slipfork --help` to see the available surface.";

/// Parses `argv` with clap, then dispatches the one parsed command.
pub fn run(argv: Vec<OsString>, session: Session) -> ExitCode {
    let command = Cli::command().color(choice(session.color()));
    let matches = match command.try_get_matches_from(&argv) {
        Ok(matches) => matches,
        Err(error) => return usage_failure(error, &argv, session.json_requested()),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => return usage_failure(error, &argv, session.json_requested()),
    };
    if cli.manpage {
        return emit(&super::manpage::render());
    }
    // clap is the source of truth once parsing succeeded; the raw scan only covers parse failures.
    let session = if cli.json {
        session.force_json()
    } else {
        session
    };
    dispatch(cli, session)
}

fn choice(color: bool) -> ColorChoice {
    if color {
        ColorChoice::Always
    } else {
        ColorChoice::Never
    }
}

fn dispatch(cli: Cli, session: Session) -> ExitCode {
    let json = cli.json;
    let verbose = cli.verbose;
    let Some(command) = cli.command else {
        return session.status(json, verbose);
    };
    match command {
        Command::Status => session.status(json, verbose),
        Command::Ps => session.ps(json, verbose),
        Command::Doctor => session.doctor(json),
        Command::Up { target, features } => session.up(&UpArgs {
            json,
            target,
            features: features.features,
            no_default_features: features.no_default_features,
        }),
        Command::Down { target } => session.down(&DownArgs { json, target }),
        Command::Restart { target, service } => session.restart(&RestartArgs {
            target,
            service,
            json,
        }),
        Command::Fork { name, features } => session.fork(&ForkArgs {
            name,
            json,
            features: features.features,
            no_default_features: features.no_default_features,
        }),
        Command::Drop { name, force } => session.drop(&DropArgs { name, force, json }),
        Command::Logs {
            targets,
            follow,
            all,
            since,
            until,
            timestamps,
            run,
            runs,
        } => session.logs(&LogsArgs {
            follow,
            all,
            targets,
            since_ms: since,
            until_ms: until,
            timestamps,
            run,
            runs,
            json,
        }),
        Command::Shell { target } => session.shell(&TargetArgs { target, json }),
        Command::Exec {
            target,
            interactive,
            no_interactive,
            argv,
        } => {
            if argv.is_empty() {
                return usage_error(
                    "`exec` needs `--` before the command, e.g. `slipfork exec api -- pytest -q`",
                    "run `slipfork exec --help` to see the shipped form",
                    json,
                );
            }
            session.exec(&ExecArgs {
                target,
                json,
                argv,
                interactive,
                no_interactive,
            })
        }
        Command::Env {
            component,
            reveal,
            features,
        } => session.env(&EnvArgs {
            component,
            reveal,
            json,
            features: features.features,
            no_default_features: features.no_default_features,
        }),
        Command::Features { changes, features } => session.features(&FeaturesArgs {
            changes,
            features: features.features,
            no_default_features: features.no_default_features,
            json,
        }),
        Command::Secret { command } => {
            let (verb, target) = match command {
                SecretCommand::List => (SecretVerb::List, None),
                SecretCommand::Set { target } => (SecretVerb::Set, Some(target)),
                SecretCommand::Unset { target } => (SecretVerb::Unset, Some(target)),
            };
            session.secret(&SecretArgs { verb, target, json })
        }
        Command::Open { service } => session.open(&OpenArgs { service, json }),
        // `slipfork auth` is deferred past 0.1.0; restore this arm with the `verbs` variant.
        // Command::Auth { url, ospp_endpoint } => session.auth(&AuthArgs {
        //     endpoint: ospp_endpoint,
        //     positional: url,
        //     json,
        // }),
        Command::Fmt { path, check } => session.fmt(&FmtArgs {
            check,
            json,
            path: path.unwrap_or_else(|| PathBuf::from("Forkfile")),
        }),
        Command::Migrate { path, write, force } => session.migrate(&MigrateArgs {
            write,
            force,
            json,
            path: path.unwrap_or_else(|| PathBuf::from("snow.yml")),
        }),
        Command::Completions { shell } => {
            let mut command = Cli::command();
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut command, "slipfork", &mut script);
            // `slipfork completions bash | head` closes the pipe early; that is normal shell
            // usage, not a panic (clap_complete writes straight to stdout and unwraps).
            let mut stdout = std::io::stdout().lock();
            let result = stdout.write_all(&script).and_then(|()| stdout.flush());
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!(
                        "error: could not write completions: {error}\nnext: re-run `slipfork completions {shell}` with stdout open"
                    );
                    ExitCode::from(1)
                }
            }
        }
        Command::Help { command } => print_help(command.as_deref(), session.color(), json),
        Command::Version => emit(&format!("slipfork {}\n", env!("CARGO_PKG_VERSION"))),
        Command::Supervise { project, world } => {
            session.supervise(&SuperviseArgs { project, world })
        }
    }
}

/// Prints the overview, or one verb's long help, with the invocation's colour decision.
fn print_help(topic: Option<&str>, color: bool, json: bool) -> ExitCode {
    let mut command = Cli::command().color(choice(color));
    let Some(topic) = topic else {
        let _ = command.print_long_help();
        println!();
        return ExitCode::SUCCESS;
    };
    match command.find_subcommand_mut(topic) {
        Some(subcommand) => {
            let _ = subcommand.print_long_help();
            println!();
            ExitCode::SUCCESS
        }
        None => usage_error(&format!("unknown command `{topic}`"), USAGE_NEXT, json),
    }
}

/// Renders one usage failure as `error:` plus exactly one `next:` and exits `2`.
///
/// Under `--json` the same rejection becomes one nested `error` object with the standard remedy.
fn usage_error(message: &str, next: &str, json: bool) -> ExitCode {
    if json {
        let first = message.lines().next().unwrap_or("invalid command line");
        let first = first.strip_prefix("error: ").unwrap_or(first);
        println!(
            "{}",
            json!({ "error": { "code": "usage", "message": first, "next": USAGE_NEXT } })
        );
        return ExitCode::from(2);
    }
    eprintln!("error: {message}");
    eprintln!("next: {next}");
    ExitCode::from(2)
}

/// Renders clap's help/version output as-is, and every rejection as one line plus one remedy.
fn usage_failure(error: clap::Error, argv: &[OsString], json: bool) -> ExitCode {
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = error.print();
            ExitCode::SUCCESS
        }
        _ => {
            if argv.iter().skip(1).any(|argument| argument == "lsp") {
                return usage_error(
                    "unknown argument `lsp` (the language server is now the standalone slipfork-lsp binary)",
                    "install `slipfork-lsp` and point your editor at it",
                    json,
                );
            }
            let rendered = error.to_string();
            let first = rendered
                .lines()
                .find(|line| line.starts_with("error:"))
                .unwrap_or("error: invalid command line");
            if json {
                return usage_error(first, USAGE_NEXT, true);
            }
            eprintln!("{first}");
            eprintln!("next: {USAGE_NEXT}");
            ExitCode::from(2)
        }
    }
}
