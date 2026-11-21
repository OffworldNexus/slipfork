//! `argv` parsing for every shipped verb, including the hidden supervisor mode.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::commands::{ExecArgs, FmtArgs, LogsArgs, MigrateArgs, TargetArgs};
use crate::world::{DownArgs, RestartArgs, SuperviseArgs, UpArgs};

use super::surface::Shell;
use super::{CommandLine, DaemonInstallArgs, HelpTopic, StatusArgs, UsageError, Verb};

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
            "logs" => logs_verb(rest)?,
            "shell" => shell_verb(rest)?,
            "exec" => exec_verb(rest)?,
            "db" => db_verb(rest)?,
            "redis" => redis_verb(rest)?,
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

fn status_verb(args: &[&str], topic: HelpTopic) -> Result<Option<HelpTopic>, UsageError> {
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

fn doctor_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn daemon_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn up_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn down_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn restart_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn logs_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Logs));
    }
    let mut follow = false;
    let mut json = false;
    let mut service = None;
    for arg in args {
        match *arg {
            "-f" | "--follow" => follow = true,
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
    Ok(Verb::Logs(LogsArgs {
        follow,
        service,
        json,
    }))
}

fn shell_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Shell));
    }
    let (target, json) = target_args(args, "shell")?;
    Ok(Verb::Shell(TargetArgs { target, json }))
}

fn db_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Db));
    }
    let (target, json) = target_args(args, "db")?;
    Ok(Verb::Db(TargetArgs { target, json }))
}

fn redis_verb(args: &[&str]) -> Result<Verb, UsageError> {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Verb::Help(HelpTopic::Redis));
    }
    let (target, json) = target_args(args, "redis")?;
    Ok(Verb::Redis(TargetArgs { target, json }))
}

fn target_args(args: &[&str], verb: &str) -> Result<(Option<String>, bool), UsageError> {
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

fn exec_verb(args: &[&str]) -> Result<Verb, UsageError> {
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

fn supervise_verb(args: &[&str]) -> Result<Verb, UsageError> {
    let mut project = None;
    let mut world = None;
    let mut json = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "--json" => json = true,
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
        json,
    }))
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

fn completions_verb(args: &[&str]) -> Result<Verb, UsageError> {
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
