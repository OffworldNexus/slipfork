use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result alias for every fallible world operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Which namespace an invalid world/fork/pod name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A `worlds/<world>` world data subvolume.
    World,
    /// A `<project>/.forks/<fork>` + `worlds/<fork>--<world>` snapshot pair.
    Fork,
    /// An OCI pod id (also its UTS hostname).
    Pod,
}

impl NameKind {
    const fn label(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Fork => "fork",
            Self::Pod => "pod",
        }
    }
}

/// Everything the Btrfs engine, pod renderer, and world supervisor can refuse to do.
#[derive(Debug)]
pub enum Error {
    /// The path is not on a `BTRFS_SUPER_MAGIC` filesystem.
    NotBtrfs { path: PathBuf },
    /// The path is a plain directory instead of a subvolume root.
    NotASubvolume { path: PathBuf },
    /// The path exists but is not a directory.
    NotADirectory { path: PathBuf },
    /// The path has no final component to derive sibling paths from.
    UnsupportedPath { path: PathBuf },
    /// A world/fork/pod name violates `[a-z0-9][a-z0-9._-]*`.
    InvalidName { kind: NameKind, name: String },
    /// `drop`/`destroy` met uncommitted changes without `--force`.
    RefusedDirtyCheckout { target: String },
    /// A fork checkout copy met a mount point nested inside the checkout.
    NestedMount { path: PathBuf },
    /// A fork checkout copy met a FIFO, socket, or device node it cannot copy.
    UnsupportedEntry { path: PathBuf },
    /// The tmpfs-only pod runtime directory sits on persistent disk.
    RuntimeNotTmpfs { path: PathBuf },
    /// A required host tool is missing.
    MissingTool { tool: &'static str, path: PathBuf },
    /// A pod value failed validation.
    InvalidPod { id: String, reason: String },
    /// An S3 bucket name is not a legal bucket.
    InvalidBucket { name: String },
    /// The init handshake or the world namespaces failed.
    Init { message: String },
    /// The container runtime rejected a pod launch inside a booted world.
    PodLaunch { message: String },
    /// The NDJSON control protocol was violated.
    Protocol { message: String },
    /// The world's `slirp4netns` forwarder exited, or its `tap0` never came up.
    Egress {
        /// Forwarder exit code, `None` when it is still running but never attached.
        status: Option<i32>,
        message: String,
    },
    /// `/proc/<init-pid>/ns/*` did not match the observed namespace identities.
    NamespaceMismatch {
        kind: String,
        expected: String,
        actual: String,
    },
    /// An operation missed its deadline.
    Timeout { action: &'static str },
    /// A resource pod never answered on its endpoint (loopback port or unix socket).
    NotReady {
        tool: &'static str,
        endpoint: String,
    },
    /// The golden template exists but is neither sealed nor version-stamped.
    StaleTemplate { path: PathBuf },
    /// Another `slipfork up` kept the golden-template lock past the wait budget.
    TemplateWait { path: PathBuf },
    /// Teardown left processes alive in the world's process group.
    LeftoverProcesses { count: usize, pgid: i32 },
    /// The world boot DAG contains a dependency cycle.
    BootCycle { steps: Vec<String> },
    /// One world boot step failed and left the world running.
    Boot {
        step: String,
        line: u32,
        message: String,
        /// Concrete next action when `Forkfile` line numbers are not the remedy.
        remedy: Option<String>,
    },
    /// The pre-fork PostgreSQL `CHECKPOINT` over `control.sock` failed.
    PostgresCheckpoint { pod: String, message: String },
    /// A JSON control/config payload could not be encoded or decoded.
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The pre-snapshot `syncfs(2)` on the store's filesystem failed.
    FlushStore { path: PathBuf, source: io::Error },
    /// A filesystem or ioctl operation failed.
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl Error {
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBtrfs { path } => write!(
                f,
                "`{}` is not on Btrfs\nnext: mount the drive with `-o user_subvol_rm_allowed,noatime`; Slipfork needs Btrfs for CoW forks",
                path.display()
            ),
            Self::NotASubvolume { path } => write!(
                f,
                "`{}` is not a Btrfs subvolume Slipfork can snapshot\nnext: remove or move aside that entry, then retry; Slipfork only treats subvolumes it created as its own",
                path.display()
            ),
            Self::NotADirectory { path } => write!(
                f,
                "`{}` is not a directory\nnext: point Slipfork at the project root directory",
                path.display()
            ),
            Self::UnsupportedPath { path } => write!(
                f,
                "`{}` has no final path component Slipfork can create siblings for\nnext: pass an absolute project path below the Btrfs mount",
                path.display()
            ),
            Self::InvalidName { kind, name } => {
                let kind = kind.label();
                write!(
                    f,
                    "invalid {kind} name `{name}`\nnext: use lowercase letters, digits, `.`, `_`, `-`, starting with a letter or digit"
                )
            }
            Self::RefusedDirtyCheckout { target } => write!(
                f,
                "`{target}` has uncommitted changes\nnext: commit or stash them, or re-run with `--force` to discard the checkout"
            ),
            Self::RuntimeNotTmpfs { path } => write!(
                f,
                "`{}` is not on tmpfs\nnext: export XDG_RUNTIME_DIR to a tmpfs such as /run/user/$UID so pod secrets never reach disk",
                path.display()
            ),
            Self::NestedMount { path } => write!(
                f,
                "`{}` is a mount point inside the checkout\nnext: unmount it and re-run `slipfork fork`; copying a checkout never spans a mount",
                path.display()
            ),
            Self::UnsupportedEntry { path } => write!(
                f,
                "`{}` is a FIFO, socket, or device node\nnext: remove it and re-run `slipfork fork`; only directories, files, and symlinks are copied into a fork",
                path.display()
            ),
            Self::MissingTool { tool, path } => write!(
                f,
                "`{tool}` is missing at `{}`\nnext: install {tool}, or reinstall slipfork (or your distro package) if it should ship with it",
                path.display()
            ),
            Self::InvalidPod { id, reason } => write!(
                f,
                "invalid pod `{id}`: {reason}\nnext: use lowercase ids and absolute in-container paths"
            ),
            Self::InvalidBucket { name } => write!(
                f,
                "invalid S3 bucket `{name}`\nnext: use 3-63 lowercase letters, digits, `.`, `-`, starting and ending alphanumeric"
            ),
            Self::Init { message } => write!(
                f,
                "world init failed: {message}\nnext: check `unshare --user --map-current-user -- true` works for this user"
            ),
            Self::PodLaunch { message } => write!(
                f,
                "{message}\nnext: install crun (e.g. `apt install crun`) and confirm `crun --version` runs, then retry"
            ),
            Self::Protocol { message } => write!(
                f,
                "control protocol error: {message}\nnext: {}",
                crate::control::REMEDY_PROTOCOL
            ),
            Self::Egress { status, message } => {
                let status = status.map_or_else(String::new, |code| format!(", exit {code}"));
                write!(
                    f,
                    "world egress `slirp4netns` failed{status}: {message}\nnext: install slirp4netns (and retry); pods cannot reach PyPI, npm, or HTTPS without it"
                )
            }
            Self::NamespaceMismatch {
                kind,
                expected,
                actual,
            } => write!(
                f,
                "world `{kind}` namespace identity changed (`{expected}` -> `{actual}`)\nnext: stop the world and boot it again"
            ),
            Self::Timeout { action } => write!(
                f,
                "timed out trying to {action}\nnext: inspect `dmesg` for unprivileged user-namespace denials"
            ),
            Self::NotReady { tool, endpoint } => write!(
                f,
                "`{tool}` did not answer on `{endpoint}`\nnext: run `slipfork logs -a`"
            ),
            Self::StaleTemplate { path } => write!(
                f,
                "the golden template `{}` is neither sealed nor version-stamped\nnext: delete it with `btrfs subvolume delete {}` and re-run `slipfork up`",
                path.display(),
                path.display()
            ),
            Self::TemplateWait { path } => write!(
                f,
                "another `slipfork up` is still building the golden template `{}`\nnext: wait for it to finish, then re-run `slipfork up`",
                path.display()
            ),
            Self::LeftoverProcesses { count, pgid } => write!(
                f,
                "{count} process(es) survived teardown in process group {pgid}\nnext: kill the group with `pkill -g {pgid}` and re-run"
            ),
            Self::BootCycle { steps } => write!(
                f,
                "world boot dependency cycle: {}\nnext: remove the circular `@<tool>` or hook dependency and re-run `slipfork up`",
                steps.join(" -> ")
            ),
            Self::Boot {
                step,
                line,
                message,
                ..
            } => write!(
                f,
                "`{step}` failed at Forkfile:{line}: {message}\nnext: fix the step and re-run `slipfork up`; the world stays up for inspection"
            ),
            Self::PostgresCheckpoint { pod, message } => write!(
                f,
                "could not checkpoint postgres pod `{pod}` before the fork: {message}\nnext: run `slipfork logs -a` and retry the fork"
            ),
            Self::Json { path, .. } => write!(
                f,
                "could not encode or decode JSON for `{}`\nnext: remove the stale runtime file and re-run",
                path.display()
            ),
            Self::FlushStore { path, source } => write!(
                f,
                "could not flush pending writes on `{}`: {source}\nnext: re-run the command; if it persists, check `dmesg` and `btrfs device stats` for I/O errors",
                path.display()
            ),
            // The errno stays in the message: the lab only ever shows `Display`.
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "could not {action} `{}`: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FlushStore { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}
