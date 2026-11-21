//! Host verbs: `doctor`, `daemon`, `env`, `secret`, `open`, and `auth`.

use crate::commands::{AuthArgs, EnvArgs, OpenArgs, SecretArgs, SecretVerb};

use super::super::{DaemonInstallArgs, HelpTopic, StatusArgs, UsageError, Verb};
use super::target_args;

pub(super) fn doctor_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Doctor));
    }
    let mut json = false;
    for arg in args {
        match *arg {
            "--json" => json = true,
            other => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `doctor`"
                )));
            }
        }
    }
    Ok(Verb::Doctor(StatusArgs {
        json,
        verbose: false,
    }))
}

pub(super) fn daemon_verb(args: &[&str]) -> Result<Verb, UsageError> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(UsageError::new(
            "`daemon` needs a subcommand: `daemon install`",
        ));
    };
    match *sub {
        "-h" | "--help" => Ok(Verb::Help(HelpTopic::DaemonInstall)),
        "install" => daemon_install_verb(rest),
        other => Err(UsageError::new(format!(
            "unknown daemon command `{other}` (expected `install`)"
        ))),
    }
}

fn daemon_install_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::DaemonInstall));
    }
    let mut ingress = false;
    let mut json = false;
    for arg in args {
        match *arg {
            "--ingress" => ingress = true,
            "--json" => json = true,
            other => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `daemon install`"
                )));
            }
        }
    }
    Ok(Verb::DaemonInstall(DaemonInstallArgs { ingress, json }))
}

pub(super) fn env_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Env));
    }
    let mut reveal = false;
    let mut json = false;
    let mut component = None;
    for arg in args {
        match *arg {
            "--reveal" => reveal = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!("unknown flag `{other}` for `env`")));
            }
            other => {
                if component.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`env` takes at most one component"));
                }
            }
        }
    }
    Ok(Verb::Env(EnvArgs {
        component,
        reveal,
        json,
    }))
}

pub(super) fn secret_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Secret));
    }
    let Some((sub, rest)) = args.split_first() else {
        return Err(UsageError::new(
            "`secret` needs a subcommand: list, set, or unset",
        ));
    };
    let verb = match *sub {
        "list" => SecretVerb::List,
        "set" => SecretVerb::Set,
        "unset" => SecretVerb::Unset,
        other => {
            return Err(UsageError::new(format!(
                "unknown secret command `{other}` (expected list, set, or unset)"
            )));
        }
    };
    let mut json = false;
    let mut target = None;
    for arg in rest {
        match *arg {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `secret`"
                )));
            }
            other => {
                if target.replace((*other).to_owned()).is_some() {
                    return Err(UsageError::new("`secret` takes at most one component.KEY"));
                }
            }
        }
    }
    Ok(Verb::Secret(SecretArgs { verb, target, json }))
}

pub(super) fn open_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Open));
    }
    let (service, json) = target_args(args, "open")?;
    Ok(Verb::Open(OpenArgs { service, json }))
}

pub(super) fn auth_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Auth));
    }
    let mut endpoint = None;
    let mut positional = None;
    let mut json = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "--json" => json = true,
            "--ospp-endpoint" => {
                let value = iter
                    .next()
                    .ok_or_else(|| UsageError::new("`--ospp-endpoint` needs a URL"))?;
                endpoint = Some((*value).to_owned());
            }
            other if other.starts_with('-') => {
                return Err(UsageError::new(format!(
                    "unknown flag `{other}` for `auth`"
                )));
            }
            other => {
                if positional.replace(other.to_owned()).is_some() {
                    return Err(UsageError::new("`auth` takes at most one URL"));
                }
            }
        }
    }
    Ok(Verb::Auth(AuthArgs {
        endpoint,
        positional,
        json,
    }))
}
