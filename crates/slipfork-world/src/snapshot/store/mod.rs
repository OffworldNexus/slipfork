//! `SnapshotStore`: layout, records, per-key gates and the reconcile/GC entry points (§6.2).

mod lock;
mod reconcile;
mod reconcile_serial;
mod record;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

pub(crate) use lock::LOCK_FILE;
pub use reconcile_serial::Observed;
pub use record::{Facts, NodeRecord, RECORD_VERSION};

use super::{NodeKey, SnapshotError, SnapshotResult};
use crate::layout::StoreLayout;
use crate::port::{Dirtiness, WorldEngine};

/// Whether reconcile reuses sealed nodes or rebuilds every materialized node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    /// Reuse every sealed, recorded node (the default).
    Warm,
    /// Rebuild every materialized node (`--no-cache`), exchanging same-key twins.
    Cold,
}

/// Bounded wave width; clamped to at least one worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parallelism(usize);

impl Parallelism {
    /// Clamps `workers` to at least one.
    #[must_use]
    pub fn new(workers: usize) -> Self {
        Self(workers.max(1))
    }

    /// The clamped worker count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Default for Parallelism {
    fn default() -> Self {
        Self(1)
    }
}

/// The in-memory twin of a lease: registered before wave 1, dropped on failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    head: NodeKey,
    keys: BTreeSet<NodeKey>,
}

impl Pin {
    /// Pins `head` plus every desired key in its parent closure.
    #[must_use]
    pub fn new(head: NodeKey, keys: BTreeSet<NodeKey>) -> Self {
        Self { head, keys }
    }

    /// The head this pin protects.
    #[must_use]
    pub const fn head(&self) -> NodeKey {
        self.head
    }

    /// Every pinned key.
    #[must_use]
    pub const fn keys(&self) -> &BTreeSet<NodeKey> {
        &self.keys
    }
}

/// A successful reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// The head now sealed or recorded.
    pub head: NodeKey,
    /// Sealed nodes built by this call (record-only nodes never count).
    pub built: usize,
    /// Sealed nodes reused by this call (record-only nodes never count).
    pub reused: usize,
    /// Nodes gc swept after the call.
    pub pruned: usize,
    /// The pin the daemon keeps until the supervisor wrote the lease.
    pub pin: Pin,
    /// Sealed keys built by this call, in build order.
    pub built_keys: Vec<NodeKey>,
    /// Sealed keys reused by this call, in wave order.
    pub reused_keys: Vec<NodeKey>,
}

/// The first error of a reconcile, after siblings finished and own slots were destroyed.
///
/// The run id that `next: slipfork logs -r <id>` needs is minted in slice 3 by the daemon
/// `Reconciler` (the world's run catalog); this value deliberately carries none.
#[derive(Debug)]
pub struct ReconcileFailed {
    /// The node that failed.
    pub node: NodeKey,
    /// Its source line.
    pub line: u32,
    /// The failure.
    pub cause: Box<SnapshotError>,
    /// Nodes sealed before the failure (a retry resumes after them).
    pub built: usize,
    /// Nodes reused before the failure.
    pub reused: usize,
}

impl fmt::Display for ReconcileFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.node.as_hex();
        write!(
            f,
            "node {} failed at line {}: {}\nnext: fix that step and re-run `slipfork up`; sealed nodes are reused",
            &hex[..12],
            self.line,
            self.cause
        )
    }
}

impl std::error::Error for ReconcileFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// One sealed-node store for one Btrfs `<store>/snapshots/` directory.
pub struct SnapshotStore<'a> {
    pub(crate) layout: &'a StoreLayout,
    pub(crate) engine: &'a dyn WorldEngine,
    pub(crate) root: PathBuf,
    pub(crate) counter: AtomicU64,
    pub(crate) gates: Mutex<BTreeMap<NodeKey, Arc<Mutex<()>>>>,
}

impl<'a> SnapshotStore<'a> {
    /// Binds the store to one layout and one engine; nothing is read from the environment.
    #[must_use]
    pub fn new(layout: &'a StoreLayout, engine: &'a dyn WorldEngine) -> Self {
        Self {
            layout,
            engine,
            root: layout.snapshots_root(),
            counter: AtomicU64::new(0),
            gates: Mutex::new(BTreeMap::new()),
        }
    }

    /// `<store>/snapshots`.
    #[must_use]
    pub fn snapshots_root(&self) -> &Path {
        &self.root
    }

    /// `<store>/snapshots/<key>`.
    #[must_use]
    pub fn node_dir(&self, key: &NodeKey) -> PathBuf {
        self.root.join(key.as_hex())
    }

    /// `<store>/snapshots/<key>.json`.
    #[must_use]
    pub fn record_path(&self, key: &NodeKey) -> PathBuf {
        self.root.join(format!("{}.json", key.as_hex()))
    }

    pub(crate) fn next_ordinal(&self) -> u64 {
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn destroy_path(&self, path: &Path) -> SnapshotResult<()> {
        if !path.exists() {
            return Ok(());
        }
        if crate::btrfs::is_subvolume_path(path).unwrap_or(false) {
            self.engine.destroy(path, Dirtiness::Clean, true)?;
        } else {
            fs::remove_dir_all(path).map_err(|source| SnapshotError::io("remove", path, source))?;
        }
        Ok(())
    }
}
