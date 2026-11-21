//! Meta verbs: `fmt`, `migrate`, and `completions`.

use std::path::PathBuf;

use crate::commands::{FmtArgs, MigrateArgs};
use crate::surface::Shell;

use super::super::{HelpTopic, UsageError, Verb};

pub(super) fn fmt_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

pub(super) fn migrate_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

pub(super) fn completions_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Completions));
    }
    let Some((shell, rest)) = args.split_first() else {
        return Err(UsageError::new(
            "`completions` needs a shell: bash, zsh, or fish",
        ));
    };
    if let Some(extra) = rest.first() {
        return Err(UsageError::new(format!(
            "unknown argument `{extra}` for `completions`"
        )));
    }
    Shell::parse(shell).map(Verb::Completions).ok_or_else(|| {
        UsageError::new(format!(
            "`{shell}` is not a shell; expected bash, zsh, or fish"
        ))
    })
}
