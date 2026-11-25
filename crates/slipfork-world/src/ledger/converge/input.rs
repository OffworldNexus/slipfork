//! Ledger row and observation inputs the pure planner consumes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::super::entity::WorldState;

/// One `worlds` row as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRow {
    /// Row id.
    pub id: i64,
    /// Absolute checkout path.
    pub checkout_path: PathBuf,
    /// `<basename>` for a root, `<fork>--<root>` for a fork.
    pub world_name: String,
    /// `<mount>/.slipfork-store`.
    pub store_root: PathBuf,
    /// Lifecycle state.
    pub state: WorldState,
    /// Supervisor pid, when recorded.
    pub supervisor_pid: Option<i64>,
    /// `<runtime_dir>/slipfork/pods/<world>`, when recorded.
    pub pods_dir: Option<PathBuf>,
    /// When the current boot published `live`.
    pub started_at_ms: Option<i64>,
    /// One-line convergence problem for a `broken` row.
    pub problem: Option<String>,
    /// Last mutation, epoch milliseconds.
    pub updated_at_ms: i64,
}

/// One `forks` row as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkRow {
    /// `worlds.id` of this fork.
    pub world_id: i64,
    /// The family root.
    pub root_id: i64,
    /// The immediate parent.
    pub parent_id: i64,
    /// `<root>/.forks/<name>`.
    pub name: String,
    /// Git branch, empty until `up`.
    pub branch: String,
}

/// Every row the ledger knows, as plain data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerRows {
    /// Roots and forks, ordered by id.
    pub worlds: Vec<WorldRow>,
    /// Lineage edges, ordered by `world_id`.
    pub forks: Vec<ForkRow>,
}

impl LedgerRows {
    /// No roots and no forks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.worlds.is_empty() && self.forks.is_empty()
    }
}

/// The state of `<world>/.slipfork-lease.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeaseState {
    /// No lease file; normal for `stopped`.
    #[default]
    Absent,
    /// A lease parsed cleanly.
    Present,
    /// The lease exists but cannot be read.
    Corrupt,
}

/// What the disk and `/proc` prove about one world.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorldFacts {
    /// The checkout path exists.
    pub checkout_exists: bool,
    /// `worlds/<world>` (plus flat `.kind.pod` clones) exists.
    pub world_data_exists: bool,
    /// `/proc/<supervisor_pid>` is alive.
    pub supervisor_alive: bool,
    /// Lease file state.
    pub lease: LeaseState,
    /// Existing pod directories for this world.
    pub pod_dirs: Vec<PathBuf>,
}

/// What an untracked `<root>/.forks/<name>` directory looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UntrackedKind {
    /// An empty `ino == 2` Btrfs stub left by a snapshot.
    Stub,
    /// A `.partial-*` stage left mid-copy.
    Partial,
    /// A real checkout directory.
    Checkout {
        /// `<name>/Forkfile` exists.
        has_forkfile: bool,
        /// Parent recovered from `Lease.origin`.
        parent_hint: Option<String>,
    },
}

/// One `<root>/.forks/<name>` directory with no `forks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisteredFork {
    /// The family root id.
    pub root_id: i64,
    /// The `.forks/<name>` entry.
    pub name: String,
    /// Its absolute path.
    pub checkout_path: PathBuf,
    /// What the directory contains.
    pub kind: UntrackedKind,
}

/// One `worlds/<fork>--<root>*` subvolume with no checkout and no row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanWorld {
    /// The store the subvolume lives under.
    pub store_root: PathBuf,
    /// The `<fork>--<root>` subvolume prefix.
    pub name: String,
}

/// Ephemeral pod directories for a world the ledger has no row for (`specs/03` §7.4 row 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanPods {
    /// The store the world's data would live under.
    pub store_root: PathBuf,
    /// The world id the pod directories belong to.
    pub world_name: String,
    /// Every existing pod directory observed for that world.
    pub pod_dirs: Vec<PathBuf>,
}

/// Everything `plan_converge` may consider.
#[derive(Debug, Clone, Default)]
pub struct ConvergeInput {
    /// Ledger rows.
    pub worlds: Vec<WorldRow>,
    /// Lineage edges.
    pub forks: Vec<ForkRow>,
    /// Observed per-world facts, keyed by `worlds.id`.
    pub facts: BTreeMap<i64, WorldFacts>,
    /// `.forks` entries the ledger does not know.
    pub untracked: Vec<UnregisteredFork>,
    /// Unregistered subvolumes under `worlds/`.
    pub orphan_worlds: Vec<OrphanWorld>,
    /// Ephemeral pod directories for worlds the ledger does not know.
    pub orphan_pods: Vec<OrphanPods>,
}
