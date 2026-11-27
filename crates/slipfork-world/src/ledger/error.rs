//! Every refusal the fork ledger can produce: one line plus a `next:` remedy.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result alias for the fork ledger.
pub type LedgerResult<T> = std::result::Result<T, LedgerError>;

/// Everything `state.db` or target resolution can refuse to do.
#[derive(Debug)]
pub enum LedgerError {
    /// A filesystem, lock, or runtime operation failed.
    Io {
        /// What was attempted.
        action: &'static str,
        /// Target path.
        path: PathBuf,
        /// Underlying errno.
        source: io::Error,
    },
    /// The database rejected an operation.
    Db {
        /// `sea-orm` or SQLite detail.
        message: String,
    },
    /// Another process holds the single-writer lock.
    WriterBusy,
    /// This handle was opened read-only.
    ReadOnly,
    /// `state.db` carries a migration this binary does not know.
    SchemaNewer {
        /// The unknown applied version.
        version: String,
    },
    /// The reader cannot serve rows before the writer migrates.
    PendingMigration {
        /// How many known migrations are unapplied.
        count: usize,
    },
    /// A unique key rejected the row.
    Duplicate,
    /// `UNIQUE(store_root, world_name)` rejected the row.
    NameTaken {
        /// The world id that already exists.
        world_name: String,
    },
    /// `delete_world_row` was handed a root row.
    RootRow {
        /// The root world that cannot be deleted.
        world_name: String,
    },
    /// A state update matched no row.
    UnknownRow {
        /// The requested row id.
        id: i64,
    },
    /// No world in the ledger matches the needle.
    UnknownTarget {
        /// What the caller typed.
        needle: String,
    },
    /// A bare needle matched more than one family.
    AmbiguousTarget {
        /// What the caller typed.
        needle: String,
        /// How many worlds matched.
        count: usize,
        /// Fork world ids (`<fork>--<root>`) among the matches, sorted.
        forks: Vec<String>,
    },
    /// A parent row belongs to a different root family.
    ForeignParent {
        /// The family root id.
        root_id: i64,
        /// The rejected parent id.
        parent_id: i64,
    },
    /// A derived path or name exceeds its kernel or schema budget.
    NameTooLong {
        /// What the value describes (`world name`, `control socket path`).
        kind: &'static str,
        /// The rejected value.
        name: String,
        /// The byte budget.
        limit: usize,
    },
    /// A fork may not reuse its root world's name.
    NameCollision {
        /// The colliding fork name.
        name: String,
    },
    /// A caller-injected layout name or protocol value was invalid.
    Invalid {
        /// One-line detail, already carrying its own `next:` line.
        message: String,
    },
}

impl LedgerError {
    /// Builds an I/O error for `path`.
    pub fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Wraps a `sea-orm` error.
    pub(crate) fn db(error: impl fmt::Display) -> Self {
        Self::Db {
            message: error.to_string(),
        }
    }
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => write!(
                f,
                "could not {action} `{}`: {source}\nnext: check free space and permissions under the state directory, then re-run",
                path.display()
            ),
            Self::Db { message } => write!(
                f,
                "the ledger database rejected an operation: {message}\nnext: run `slipfork ps`; if it persists, inspect the log and re-run `slipfork up`"
            ),
            Self::WriterBusy => f.write_str(
                "another slipfork process holds the ledger writer lock\nnext: slipfork ps",
            ),
            Self::ReadOnly => f.write_str(
                "this ledger handle is read-only\nnext: run the command from the daemon, or `slipfork up` to migrate",
            ),
            Self::SchemaNewer { version } => write!(
                f,
                "`state.db` was written by a newer slipfork (unknown migration `{version}`)\nnext: upgrade slipfork"
            ),
            Self::PendingMigration { count } => write!(
                f,
                "`state.db` has {count} pending migration(s)\nnext: slipfork up"
            ),
            Self::Duplicate => f.write_str(
                "the ledger already holds a row with the same key\nnext: run `slipfork ps` to find the existing world",
            ),
            Self::NameTaken { world_name } => write!(
                f,
                "`{world_name}` is already registered in this store\nnext: slipfork down {world_name}"
            ),
            Self::RootRow { world_name } => write!(
                f,
                "`{world_name}` is a root world\nnext: slipfork down {world_name} (roots are never deleted)"
            ),
            Self::UnknownRow { id } => write!(
                f,
                "no ledger row has id {id}\nnext: run `slipfork ps`"
            ),
            Self::UnknownTarget { needle } => write!(
                f,
                "no world matches `{needle}`\nnext: run `slipfork ps` to list worlds"
            ),
            Self::AmbiguousTarget {
                needle,
                count,
                forks,
            } => {
                if let Some(first) = forks.first() {
                    write!(
                        f,
                        "`{needle}` matches {count} worlds; fork targets: {}\nnext: slipfork drop {first}",
                        forks.join(", ")
                    )
                } else {
                    write!(
                        f,
                        "`{needle}` matches {count} root worlds\nnext: run slipfork ps"
                    )
                }
            }
            Self::ForeignParent { root_id, parent_id } => write!(
                f,
                "parent world {parent_id} is not in the family of root {root_id}\nnext: run slipfork ps"
            ),
            Self::NameTooLong { kind, name, limit } => write!(
                f,
                "`{name}` is too long for a {kind} ({limit} bytes max)\nnext: choose a shorter name"
            ),
            Self::NameCollision { name } => write!(
                f,
                "fork name `{name}` collides with its root world's name\nnext: choose another fork name"
            ),
            Self::Invalid { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<crate::Error> for LedgerError {
    fn from(error: crate::Error) -> Self {
        Self::Invalid {
            message: error.to_string(),
        }
    }
}
