//! Typed `Dockerfile` instruction model plus canonical rendering for cache keys and errors.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where a stage's root filesystem comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageBase {
    /// A registry reference resolved through the OCI puller.
    Image(String),
    /// A previously declared `AS` stage, reused as a sealed rootfs.
    Stage(String),
}

/// A normalized `COPY`/`ADD` instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySpec {
    /// `--from=<stage>`; copies from that stage's sealed rootfs instead of the build context.
    pub from: Option<String>,
    /// Raw `--chown` spec (`name`, `name:group`, `uid`, `uid:gid`), resolved against the
    /// destination stage's rootfs when the copy is applied.
    pub chown: Option<String>,
    /// `--chmod` mode, parsed as octal and masked to `0o7777`.
    pub mode: Option<u32>,
    /// Expanded source paths relative to the context or the source stage rootfs.
    pub sources: Vec<String>,
    /// Destination path inside the rootfs.
    pub dest: String,
}

/// Numeric ownership applied to copied entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    /// Container uid.
    pub uid: u32,
    /// Container gid.
    pub gid: u32,
}

/// Ownership and mode a `COPY`/`ADD` applies to every entry it writes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CopyOptions {
    /// Host owner: a single-mapping build can only represent the invoking uid/gid.
    pub(crate) owner: Owner,
    /// `--chmod` mode, masked to `0o7777`.
    pub(crate) mode: Option<u32>,
}

impl CopyOptions {
    /// Bundles the host owner with an optional `--chmod` mode.
    pub(crate) const fn new(owner: Owner, mode: Option<u32>) -> Self {
        Self { owner, mode }
    }
}

/// One `RUN --mount=type=cache` mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMount {
    /// Cache identity (`id=`), defaulting to the target path.
    pub id: Option<String>,
    /// Absolute path inside the container.
    pub target: String,
    /// Whether the cache is mounted read-only.
    pub readonly: bool,
}

/// `RUN` flags plus the command to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    /// Declared `--mount=type=cache` directories.
    pub mounts: Vec<CacheMount>,
    /// Shell or exec form.
    pub command: Command,
}

/// Shell-form or JSON-exec-form command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// `/bin/sh -c <script>`.
    Shell(String),
    /// Direct argv execution.
    Exec(Vec<String>),
}

impl Command {
    /// Canonical one-line rendering for cache keys and errors.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Shell(script) => format!("sh -c {script:?}"),
            Self::Exec(argv) => format!("exec {argv:?}"),
        }
    }

    /// The command as the `Dockerfile` author wrote it: shell scripts verbatim, exec argv joined by
    /// spaces. Never wrapped in an extra `sh -c`.
    #[must_use]
    pub fn as_written(&self) -> String {
        match self {
            Self::Shell(script) => script.clone(),
            Self::Exec(argv) => argv.join(" "),
        }
    }

    /// The argv this command expands to at exec time; shell form runs through `/bin/sh -c`.
    #[must_use]
    pub fn to_argv(&self) -> Vec<String> {
        match self {
            Self::Shell(script) => {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()]
            }
            Self::Exec(argv) => argv.clone(),
        }
    }
}

/// Port protocol for `EXPOSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    /// `tcp`.
    Tcp,
    /// `udp`.
    Udp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

/// One `EXPOSE` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// Port number.
    pub number: u16,
    /// Transport protocol.
    pub protocol: Protocol,
}

/// A parsed `Dockerfile` instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    /// `FROM <image|stage> [AS <alias>]` (the base is consumed by the stage, never applied).
    From {
        /// Registry reference or earlier stage name.
        base: StageBase,
        /// `AS` alias, lowercased.
        alias: Option<String>,
    },
    /// `ARG NAME[=default]`.
    Arg {
        /// Argument name.
        name: String,
        /// Fallback when the caller declared no value.
        default: Option<String>,
    },
    /// `ENV KEY=VALUE ...`.
    Env(Vec<(String, String)>),
    /// `WORKDIR <path>`.
    Workdir(String),
    /// `COPY [--from] [--chown] <src>... <dest>`.
    Copy(CopySpec),
    /// `ADD [--chown] <src>... <dest>`.
    Add(CopySpec),
    /// `RUN [--mount=type=cache,...] <command|heredoc>`.
    Run(RunSpec),
    /// `USER <uid>[:<gid>]`.
    User(String),
    /// `EXPOSE <port>[/tcp|udp] ...`.
    Expose(Vec<Port>),
    /// `CMD ...`.
    Cmd(Command),
    /// `ENTRYPOINT ...`.
    Entrypoint(Command),
}

impl Instruction {
    /// The instruction keyword, used as a run-record phase (`run`, `copy`, `from`, …).
    #[must_use]
    pub fn phase(&self) -> &'static str {
        match self {
            Self::From { .. } => "from",
            Self::Arg { .. } => "arg",
            Self::Env(_) => "env",
            Self::Workdir(_) => "workdir",
            Self::Copy(_) => "copy",
            Self::Add(_) => "add",
            Self::Run(_) => "run",
            Self::User(_) => "user",
            Self::Expose(_) => "expose",
            Self::Cmd(_) => "cmd",
            Self::Entrypoint(_) => "entrypoint",
        }
    }

    /// The uppercase instruction keyword (`RUN`, `COPY`, …) for live build rows.
    #[must_use]
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::From { .. } => "FROM",
            Self::Arg { .. } => "ARG",
            Self::Env(_) => "ENV",
            Self::Workdir(_) => "WORKDIR",
            Self::Copy(_) => "COPY",
            Self::Add(_) => "ADD",
            Self::Run(_) => "RUN",
            Self::User(_) => "USER",
            Self::Expose(_) => "EXPOSE",
            Self::Cmd(_) => "CMD",
            Self::Entrypoint(_) => "ENTRYPOINT",
        }
    }

    /// The instruction text without its keyword, exactly as the author wrote it.
    #[must_use]
    pub fn command_text(&self) -> String {
        match self {
            Self::Run(run) => run.command.as_written(),
            Self::Cmd(command) | Self::Entrypoint(command) => command.as_written(),
            _ => self
                .render()
                .split_once(' ')
                .map_or_else(String::new, |(_, rest)| rest.to_owned()),
        }
    }

    /// Canonical text used in cache keys and failure messages.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::From { base, alias } => {
                let base = match base {
                    StageBase::Image(image) => image.as_str(),
                    StageBase::Stage(stage) => stage.as_str(),
                };
                match alias {
                    Some(alias) => format!("FROM {base} AS {alias}"),
                    None => format!("FROM {base}"),
                }
            }
            Self::Arg { name, default } => match default {
                Some(default) => format!("ARG {name}={default}"),
                None => format!("ARG {name}"),
            },
            Self::Env(pairs) => {
                let pairs: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!("ENV {}", pairs.join(" "))
            }
            Self::Workdir(dir) => format!("WORKDIR {dir}"),
            Self::Copy(spec) => render_copy("COPY", spec),
            Self::Add(spec) => render_copy("ADD", spec),
            Self::Run(run) => {
                let mounts: Vec<String> = run
                    .mounts
                    .iter()
                    .map(|mount| {
                        format!(
                            "--mount=type=cache,target={}{}{}",
                            mount.target,
                            mount
                                .id
                                .as_ref()
                                .map(|id| format!(",id={id}"))
                                .unwrap_or_default(),
                            if mount.readonly { ",readonly" } else { "" }
                        )
                    })
                    .collect();
                if mounts.is_empty() {
                    format!("RUN {}", run.command.render())
                } else {
                    format!("RUN {} {}", mounts.join(" "), run.command.render())
                }
            }
            Self::User(user) => format!("USER {user}"),
            Self::Expose(ports) => {
                let ports: Vec<String> = ports
                    .iter()
                    .map(|port| format!("{}/{}", port.number, port.protocol))
                    .collect();
                format!("EXPOSE {}", ports.join(" "))
            }
            Self::Cmd(command) => format!("CMD {}", command.render()),
            Self::Entrypoint(command) => format!("ENTRYPOINT {}", command.render()),
        }
    }
}

fn render_copy(op: &str, spec: &CopySpec) -> String {
    let mut head = op.to_owned();
    if let Some(from) = &spec.from {
        head.push_str(&format!(" --from={from}"));
    }
    if let Some(chown) = &spec.chown {
        head.push_str(&format!(" --chown={chown}"));
    }
    if let Some(mode) = spec.mode {
        head.push_str(&format!(" --chmod={mode:o}"));
    }
    let mut parts = vec![head];
    parts.extend(spec.sources.iter().cloned());
    parts.push(spec.dest.clone());
    parts.join(" ")
}

/// One instruction with its source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInstruction {
    /// 1-based line of the logical instruction.
    pub line: usize,
    /// The instruction itself.
    pub instruction: Instruction,
}

/// One `FROM`-scoped build stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    /// 1-based line of the `FROM` that opened this stage.
    pub line: usize,
    /// `AS` alias (lowercased) when declared.
    pub alias: Option<String>,
    /// Base image or earlier stage.
    pub base: StageBase,
    /// Instructions after `FROM`.
    pub instructions: Vec<ParsedInstruction>,
}
