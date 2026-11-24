//! One snapshot graph: content keys, waves, sealed slots, leases and GC
//! (`specs/03-under-the-hood.md` §6).
//!
//! The store is a cohesive value plus the injected [`WorldEngine`](crate::WorldEngine) port: it
//! reads no environment, caches no global, and every path arrives from
//! [`StoreLayout`](crate::StoreLayout).

mod gc;
mod graph;
mod key;
mod lease;
mod slot;
mod store;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub use gc::{GC_GRACE_MS, GcOutcome, GcReport, gc_mark_set};
pub use graph::{BuildGraph, Node};
pub use key::{KEY_DOMAIN, NodeKey, NodeKind, normalize_canon};
pub use lease::{LEASE_FILE, Lease, LeaseEntry};
pub use slot::{PublishOutcome, SnapshotSlot};
pub use store::{
    Cache, Facts, Parallelism, Pin, RECORD_VERSION, ReconcileFailed, Reconciled, SnapshotStore,
};

/// Everything the snapshot store can refuse to do.
#[derive(Debug)]
pub enum SnapshotError {
    /// A graph, slot, or pin invariant was violated.
    Message(&'static str),
    /// The desired graph is not a valid DAG.
    Graph(String),
    /// A key string is not a 64-character lowercase hex digest.
    Key(String),
    /// A node record or snapshot directory could not be read.
    Record {
        /// Record path.
        path: PathBuf,
        /// Parser or I/O detail.
        message: String,
    },
    /// A lease is unreadable, malformed, or inconsistent.
    Lease {
        /// Lease path.
        path: PathBuf,
        /// Parser or I/O detail.
        message: String,
    },
    /// A filesystem operation failed.
    Io {
        /// What was attempted.
        action: &'static str,
        /// Target path.
        path: PathBuf,
        /// Underlying errno.
        source: io::Error,
    },
    /// The Btrfs engine refused an operation.
    Engine(crate::Error),
}

impl SnapshotError {
    /// Builds an I/O error for `path`.
    pub fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    /// A one-line invariant violation.
    #[must_use]
    pub const fn message(message: &'static str) -> Self {
        Self::Message(message)
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Graph(message) => write!(f, "invalid build graph: {message}"),
            Self::Key(value) => write!(
                f,
                "invalid node key `{value}`\nnext: keys are 64 lowercase hex characters written by slipfork; delete any hand-edited record"
            ),
            Self::Record { path, message } => write!(
                f,
                "could not read node record `{}`: {message}\nnext: delete the stale record and re-run",
                path.display()
            ),
            Self::Lease { path, message } => write!(
                f,
                "could not read world lease `{}`: {message}\nnext: fix or remove the lease; gc never sweeps while a lease is unreadable",
                path.display()
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(
                f,
                "could not {action} `{}`: {source}\nnext: check free space and permissions under the store, then re-run `slipfork up`",
                path.display()
            ),
            Self::Engine(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Engine(error) => Some(error),
            _ => None,
        }
    }
}

impl From<crate::Error> for SnapshotError {
    fn from(error: crate::Error) -> Self {
        Self::Engine(error)
    }
}

/// Result alias for the snapshot graph.
pub type SnapshotResult<T> = std::result::Result<T, SnapshotError>;
