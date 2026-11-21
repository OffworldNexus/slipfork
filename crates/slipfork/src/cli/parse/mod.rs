//! `argv` parsing for every shipped verb, including the hidden supervisor mode.

mod host;
mod lifecycle;
mod meta;

use std::ffi::OsString;

use super::{CommandLine, HelpTopic, StatusArgs, UsageError, Verb};
use host::{auth_verb, daemon_verb, doctor_verb, env_verb, open_verb, secret_verb};
use lifecycle::{
    db_verb, down_verb, drop_verb, exec_verb, fork_verb, logs_verb, redis_verb, restart_verb,
    shell_verb, supervise_verb, up_verb,
};
use meta::{completions_verb, fmt_verb, migrate_verb};

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
                verb: Verb::Status(StatusArgs {
                    json: false,
                    verbose: false,
                }),
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
            "status" => match status_verb(rest, HelpTopic::Status)? {
                Some(topic) => Verb::Help(topic),
                None => Verb::Status(status_args(rest)?),
            },
            "ps" => match status_verb(rest, HelpTopic::Ps)? {
                Some(topic) => Verb::Help(topic),
                None => Verb::Ps(status_args(rest)?),
            },
            "doctor" => doctor_verb(rest)?,
            "daemon" => daemon_verb(rest)?,
            "up" => up_verb(rest)?,
            "down" => down_verb(rest)?,
            "restart" => restart_verb(rest)?,
            "fork" => fork_verb(rest)?,
            "drop" => drop_verb(rest)?,
            "logs" => logs_verb(rest)?,
            "shell" => shell_verb(rest)?,
            "exec" => exec_verb(rest)?,
            "db" => db_verb(rest)?,
            "redis" => redis_verb(rest)?,
            "env" => env_verb(rest)?,
            "secret" => secret_verb(rest)?,
            "open" => open_verb(rest)?,
            "auth" => auth_verb(rest)?,
            "fmt" => fmt_verb(rest)?,
            "migrate" => migrate_verb(rest)?,
            "completions" => completions_verb(rest)?,
            // The server ships as its own binary; say where it went.
            "lsp" => {
                return Err(UsageError::new(
                    "unknown argument `lsp` (the language server is now the standalone slipfork-lsp binary)",
                ));
            }
            "__supervise" => supervise_verb(rest)?,
            other => return Err(UsageError::new(format!("unknown argument `{other}`"))),
        };
        Ok(Self { verb })
    }
}

pub(super) fn status_verb(
    args: &[&str],
    topic: HelpTopic,
) -> Result<Option<HelpTopic>, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Some(topic));
    }
    Ok(None)
}

fn status_args(args: &[&str]) -> Result<StatusArgs, UsageError> {
    let mut json = false;
    let mut verbose = false;
    for arg in args {
        match *arg {
            "--json" => json = true,
            "-v" | "--verbose" => verbose = true,
            other => {
                return Err(UsageError::new(format!("unknown flag `{other}`")));
            }
        }
    }
    Ok(StatusArgs { json, verbose })
}

/// Parses `[TARGET] [--json]` for `shell`, `db`, `redis`, `open`, and `exec`.
pub(super) fn target_args(args: &[&str], verb: &str) -> Result<(Option<String>, bool), UsageError> {
    let mut target = None;
    let mut json = false;
    for arg in args {
        match *arg {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `{verb}`"
                )));
            }
            other => {
                if target.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new(format!(
                        "`{verb}` takes at most one target"
                    )));
                }
            }
        }
    }
    Ok((target, json))
}
