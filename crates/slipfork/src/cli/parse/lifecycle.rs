//! Lifecycle verbs: `up`, `down`, `restart`, `fork`, `drop`, `logs`, `shell`, `exec`, and the
//! resource shells plus the hidden supervisor mode.

use std::path::PathBuf;

use crate::commands::{ExecArgs, LogsArgs, TargetArgs};
use crate::world::{DownArgs, DropArgs, ForkArgs, RestartArgs, SuperviseArgs, UpArgs};

use super::super::{HelpTopic, UsageError, Verb};
use super::target_args;

pub(super) fn up_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Up));
    }
    let mut json = false;
    let mut world = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "--json" => json = true,
            "--world" => {
                let value = iter
                    .next()
                    .ok_or_else(|| UsageError::new("`--world` needs a name"))?;
                world = Some((*value).to_owned());
            }
            other => {
                return Err(UsageError::new(format!(
                    "unknown argument `{other}` for `up`"
                )));
            }
        }
    }
    Ok(Verb::Up(UpArgs { json, world }))
}

pub(super) fn down_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Down));
    }
    let mut json = false;
    for arg in args {
        match *arg {
            "--json" => json = true,
            other => {
                return Err(UsageError::new(format!(
                    "unknown argument `{other}` for `down`"
                )));
            }
        }
    }
    Ok(Verb::Down(DownArgs { json }))
}

pub(super) fn restart_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Restart));
    }
    let mut json = false;
    let mut service = None;
    for arg in args {
        match *arg {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `restart`"
                )));
            }
            other => {
                if service.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`restart` takes at most one service"));
                }
            }
        }
    }
    Ok(Verb::Restart(RestartArgs { service, json }))
}

pub(super) fn fork_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Fork));
    }
    let mut json = false;
    let mut name = None;
    for arg in args {
        match *arg {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `fork`"
                )));
            }
            other => {
                if name.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`fork` takes at most one name"));
                }
            }
        }
    }
    Ok(Verb::Fork(ForkArgs { name, json }))
}

pub(super) fn drop_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Drop));
    }
    let mut json = false;
    let mut force = false;
    let mut name = None;
    for arg in args {
        match *arg {
            "--json" => json = true,
            "-f" | "--force" => force = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `drop`"
                )));
            }
            other => {
                if name.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`drop` takes exactly one fork name"));
                }
            }
        }
    }
    let Some(name) = name else {
        return Err(UsageError::new("`drop` needs a fork name"));
    };
    Ok(Verb::Drop(DropArgs { name, force, json }))
}

pub(super) fn logs_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Logs));
    }
    let mut follow = false;
    let mut all = false;
    let mut json = false;
    let mut service = None;
    for arg in args {
        match *arg {
            "-f" | "--follow" => follow = true,
            "-a" | "--all" => all = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `logs`"
                )));
            }
            other => {
                if service.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`logs` takes at most one service"));
                }
            }
        }
    }
    if all && service.is_some() {
        return Err(UsageError::new(
            "`--all` cannot be combined with a target name",
        ));
    }
    Ok(Verb::Logs(LogsArgs {
        follow,
        all,
        service,
        json,
    }))
}

pub(super) fn shell_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Shell));
    }
    let (target, json) = target_args(args, "shell")?;
    Ok(Verb::Shell(TargetArgs { target, json }))
}

pub(super) fn db_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Db));
    }
    let (target, json) = target_args(args, "db")?;
    Ok(Verb::Db(TargetArgs { target, json }))
}

pub(super) fn redis_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Redis));
    }
    let (target, json) = target_args(args, "redis")?;
    Ok(Verb::Redis(TargetArgs { target, json }))
}

pub(super) fn exec_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Exec));
    }
    let split = args.iter().position(|arg| *arg == "--");
    let (head, argv) = match split {
        Some(index) => (&args[..index], &args[index + 1..]),
        None => {
            return Err(UsageError::new(
                "`exec` needs `--` before the command, e.g. `slipfork exec api -- pytest -q`",
            ));
        }
    };
    let (target, json) = target_args(head, "exec")?;
    if argv.is_empty() {
        return Err(UsageError::new("`exec` needs a command after `--`"));
    }
    Ok(Verb::Exec(ExecArgs {
        target,
        json,
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
    }))
}

pub(super) fn supervise_verb(args: &[&str]) -> Result<Verb, UsageError> {
    let mut project = None;
    let mut world = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "--project" => {
                project =
                    Some(PathBuf::from(iter.next().ok_or_else(|| {
                        UsageError::new("`--project` needs a path")
                    })?));
            }
            "--world" => {
                world = Some(
                    (*iter
                        .next()
                        .ok_or_else(|| UsageError::new("`--world` needs a name"))?)
                    .to_owned(),
                );
            }
            other => {
                return Err(UsageError::new(format!(
                    "unknown argument `{other}` for `__supervise`"
                )));
            }
        }
    }
    Ok(Verb::Supervise(SuperviseArgs {
        project: project.ok_or_else(|| UsageError::new("`--project` is required"))?,
        world: world.ok_or_else(|| UsageError::new("`--world` is required"))?,
    }))
}
