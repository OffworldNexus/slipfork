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
use std::time::Duration;

pub use gc::{GC_GRACE_MS, GcOutcome, GcReport, gc_mark_set};
pub use graph::{BuildGraph, Node};
pub use key::{KEY_DOMAIN, NodeKey, NodeKind, normalize_canon};
pub use lease::{LEASE_FILE, Lease, LeaseEntry};
pub use slot::{PublishOutcome, SnapshotSlot};
pub use store::{
    Cache, Facts, Observed, Parallelism, Pin, RECORD_VERSION, ReconcileFailed, Reconciled,
    SnapshotStore,
};

/// One tail phase of a sealed node, announced before its operation starts.
///
/// The store owns the order and the clock: [`SnapshotStore::reconcile_serial`] announces each
/// phase through [`Observed::Tail`] before the syscall, so a slow seal or publish is visible on
/// the caller's live row instead of a silent gap. The parallel wave driver ignores phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailPhase {
    /// The slot is being cloned (or created) from its parent.
    Cloning,
    /// The materializer is applying the instruction.
    Executing,
    /// The slot subvolume is being sealed read-only.
    Sealing,
    /// The node record is being written.
    Recording,
    /// The sealed slot is being renamed into place (`publish`/`publish_exchange`).
    Publishing,
}

impl TailPhase {
    /// The short `-v` note label (`clone`, `exec`, `seal`, `record`, `publish`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cloning => "clone",
            Self::Executing => "exec",
            Self::Sealing => "seal",
            Self::Recording => "record",
            Self::Publishing => "publish",
        }
    }
}

/// The measured wall time of every phase of one sealed node.
///
/// `clone` is the parent clone that precedes the materializer, `exec` is the materializer itself,
/// and `seal`/`record`/`publish` are the post-materialize tail. Nothing here is guessed: the
/// serial driver times the store's own phase announcements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhaseTiming {
    /// Time spent cloning (or creating) the staging slot.
    pub clone: Duration,
    /// Time the materializer spent applying the instruction.
    pub exec: Duration,
    /// Time spent sealing the slot read-only.
    pub seal: Duration,
    /// Time spent writing the node record.
    pub record: Duration,
    /// Time spent publishing the sealed slot.
    pub publish: Duration,
}

impl PhaseTiming {
    /// The honest settled time: clone + exec + seal + record + publish.
    #[must_use]
    pub fn total(self) -> Duration {
        self.clone + self.exec + self.seal + self.record + self.publish
    }

    pub(crate) fn add(&mut self, phase: TailPhase, elapsed: Duration) {
        match phase {
            TailPhase::Cloning => self.clone += elapsed,
            TailPhase::Executing => self.exec += elapsed,
            TailPhase::Sealing => self.seal += elapsed,
            TailPhase::Recording => self.record += elapsed,
            TailPhase::Publishing => self.publish += elapsed,
        }
    }
}

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
    /// A caller-injected materializer failed (the builder preserves its own typed error).
    Materialize(String),
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
            Self::Materialize(message) => write!(
                f,
                "{message}\nnext: fix the failing build instruction and re-run `slipfork up`"
            ),
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

#[cfg(test)]
mod tests {
    use super::TailPhase;

    #[test]
    fn short_notes_cover_every_typed_phase() {
        for (phase, note) in [
            (TailPhase::Cloning, "clone"),
            (TailPhase::Executing, "exec"),
            (TailPhase::Sealing, "seal"),
            (TailPhase::Recording, "record"),
            (TailPhase::Publishing, "publish"),
        ] {
            assert_eq!(phase.as_str(), note);
        }
    }
}
