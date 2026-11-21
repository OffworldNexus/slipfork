use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result alias for every fallible world operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Which namespace an invalid world/fork name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A `worlds/<world>` world data subvolume.
    World,
    /// A `<project>/.forks/<fork>` + `worlds/<fork>--<world>` snapshot pair.
    Fork,
}

impl NameKind {
    const fn label(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Fork => "fork",
        }
    }
}

/// Everything the Btrfs engine can refuse to do.
#[derive(Debug)]
pub enum Error {
    /// The path is not on a `BTRFS_SUPER_MAGIC` filesystem.
    NotBtrfs { path: PathBuf },
    /// The path lives on a different filesystem than the injected Btrfs mount.
    CrossDevice { path: PathBuf, mount: PathBuf },
    /// The path is a plain directory instead of a subvolume root.
    NotASubvolume { path: PathBuf },
    /// The path exists but is not a directory.
    NotADirectory { path: PathBuf },
    /// The path has no final component to derive sibling paths from.
    UnsupportedPath { path: PathBuf },
    /// A world/fork name violates `[a-z0-9][a-z0-9._-]*`.
    InvalidName { kind: NameKind, name: String },
    /// `drop`/`destroy` met uncommitted changes without `--force`.
    RefusedDirtyCheckout { target: String },
    /// Leftover `.slipfork-adopt*` siblings blocked the in-place conversion.
    AdoptionCollision { path: PathBuf },
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
            Self::CrossDevice { path, mount } => write!(
                f,
                "`{}` is on a different filesystem than the Btrfs mount `{}`\nnext: move the project onto `{}`; CoW clones cannot cross filesystems",
                path.display(),
                mount.display(),
                mount.display()
            ),
            Self::NotASubvolume { path } => write!(
                f,
                "`{}` is not a Btrfs subvolume\nnext: run `slipfork up` once so the checkout is converted in place",
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
            Self::AdoptionCollision { path } => write!(
                f,
                "`{}` already has 64 `.slipfork-adopt*` siblings\nnext: remove the leftover conversion siblings and retry",
                path.display()
            ),
            // Raw errno and ioctl detail stay on `source()`/`Debug`; users get action plus path.
            Self::Io { action, path, .. } => {
                write!(f, "could not {action} `{}`", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
