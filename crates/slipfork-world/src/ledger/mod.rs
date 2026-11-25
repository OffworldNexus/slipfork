//! The transactional fork ledger: `state.db` rows, lineage, resolution, and convergence
//! (`specs/03` §7).
//!
//! [`ForkLedger`] is a pure database owner: it never touches Btrfs, `crun`, `/proc`, or the
//! environment. Paths and clock values are injected by the caller, and the only file it touches
//! is `<state_dir>/state.db` (plus its lock and WAL, owned by [`StateDb`]).

mod converge;
mod db;
pub mod entity;
mod error;
pub mod migrations;
mod patch;
mod resolve;

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::control::CONTROL_SOCKET;
use crate::layout::{ForkName, ProjectScope, StoreLayout, WorldName};
use crate::runtime_dir::RuntimePods;

use db::ForkInsert;
use entity::WorldActiveModel;
pub use entity::WorldState;

pub use converge::{
    ConvergeAction, ConvergeInput, ForkRow, LeaseState, LedgerRows, OrphanPods, OrphanWorld,
    UnregisteredFork, UntrackedKind, WorldFacts, WorldRow, plan_converge,
};
pub use db::{Access, STATE_FILE, StateDb, StatePaths};
pub use error::{LedgerError, LedgerResult};
pub use patch::WorldPatch;
pub use resolve::{Resolved, resolve_target};

/// A root `slipfork up` registers once; names stay reserved while the row exists.
#[derive(Debug, Clone)]
pub struct RootRequest {
    /// `<basename>`, or `<basename[..58]>-<4hex>` when the plain name is taken.
    pub name: WorldName,
    /// Absolute checkout path.
    pub checkout_path: PathBuf,
    /// `<mount>/.slipfork-store`.
    pub store_root: PathBuf,
    /// Epoch milliseconds.
    pub now_ms: i64,
}

/// One `slipfork fork` insertion, validated against the family before the snapshot ioctl runs.
#[derive(Debug, Clone)]
pub struct ForkRequest {
    /// The family root's row id.
    pub root_id: i64,
    /// The new fork name.
    pub name: ForkName,
    /// The immediate parent's row id; the root row itself for a first fork.
    pub parent_id: i64,
    /// `<runtime_dir>`, used to prove `control.sock` fits `sun_path`.
    pub runtime_dir: PathBuf,
    /// Git branch created for the fork.
    pub branch: String,
    /// Epoch milliseconds.
    pub now_ms: i64,
}

/// The `live` fields `publish_live` records once the supervisor is ready.
#[derive(Debug, Clone)]
pub struct LiveFork {
    /// Supervisor process group leader.
    pub supervisor_pid: i64,
    /// `<runtime_dir>/slipfork/pods/<world>`.
    pub pods_dir: PathBuf,
    /// Epoch milliseconds when boot published `live`.
    pub started_at_ms: i64,
}

/// The single `state.db` owner (`specs/03` §7.3).
#[derive(Debug)]
pub struct ForkLedger {
    db: StateDb,
}

impl ForkLedger {
    /// Opens the single writer on the daemon's already-open lock descriptor and migrates once.
    ///
    /// # Errors
    ///
    /// Refuses a second writer with [`LedgerError::WriterBusy`], an unknown applied migration
    /// with [`LedgerError::SchemaNewer`], or an I/O/SQLite failure.
    pub fn open_writer(paths: StatePaths, lock: File) -> LedgerResult<Self> {
        Ok(Self {
            db: StateDb::open_writer(paths, lock)?,
        })
    }

    /// Opens an offline reader; an absent `state.db` yields empty `list`/`resolve` results.
    ///
    /// # Errors
    ///
    /// Refuses pending migrations with [`LedgerError::PendingMigration`] and unknown applied
    /// migrations with [`LedgerError::SchemaNewer`].
    pub fn open_reader(state_dir: impl Into<PathBuf>) -> LedgerResult<Self> {
        Ok(Self {
            db: StateDb::open_reader(state_dir.into())?,
        })
    }

    /// The underlying handle, for a daemon that needs `applied_versions` for `doctor`.
    #[must_use]
    pub fn state_db(&self) -> &StateDb {
        &self.db
    }

    /// Every applied migration version.
    pub fn applied_versions(&self) -> LedgerResult<Vec<String>> {
        self.db.applied_versions()
    }

    /// Registers a root world at `stopped` (the `up` registration path).
    ///
    /// # Errors
    ///
    /// [`LedgerError::NameTaken`] when `UNIQUE(store_root, world_name)` is already held.
    pub fn register_root(&self, request: RootRequest) -> LedgerResult<WorldRow> {
        if request.name.as_str().len() > 63 {
            return Err(LedgerError::NameTooLong {
                kind: "world name",
                name: request.name.as_str().to_owned(),
                limit: 63,
            });
        }
        let model = self.db.insert_world(WorldActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            checkout_path: sea_orm::ActiveValue::Set(path(&request.checkout_path)),
            world_name: sea_orm::ActiveValue::Set(request.name.as_str().to_owned()),
            store_root: sea_orm::ActiveValue::Set(path(&request.store_root)),
            state: sea_orm::ActiveValue::Set(WorldState::Stopped),
            supervisor_pid: sea_orm::ActiveValue::NotSet,
            pods_dir: sea_orm::ActiveValue::NotSet,
            started_at_ms: sea_orm::ActiveValue::NotSet,
            problem: sea_orm::ActiveValue::NotSet,
            updated_at_ms: sea_orm::ActiveValue::Set(request.now_ms),
        });
        model
            .map(world_row)
            .map_err(|error| taken(error, request.name.as_str()))
    }

    /// Validates the family, `<fork>--<root>`, and `control.sock`, then inserts both rows in one
    /// transaction. The checkout is always flat `<root>/.forks/<name>`, never nested.
    ///
    /// # Errors
    ///
    /// [`LedgerError::ForeignParent`] for a parent outside the root's family,
    /// [`LedgerError::NameTooLong`] / [`LedgerError::NameCollision`] before any I/O, and
    /// [`LedgerError::NameTaken`] when the name is already held.
    pub fn begin_fork(&self, request: ForkRequest) -> LedgerResult<ForkRow> {
        let (root, _parent) = self.db.family_rows(request.root_id, request.parent_id)?;
        let root_name = WorldName::parse(&root.world_name)?;
        if request.name.as_str() == root_name.as_str() {
            return Err(LedgerError::NameCollision {
                name: request.name.as_str().to_owned(),
            });
        }
        let world_name = StoreLayout::fork_world_name(&root_name, &request.name);
        if world_name.as_str().len() > 63 {
            return Err(LedgerError::NameTooLong {
                kind: "world name",
                name: world_name.as_str().to_owned(),
                limit: 63,
            });
        }
        let socket =
            RuntimePods::world_root(&request.runtime_dir, &world_name).join(CONTROL_SOCKET);
        if socket.as_os_str().as_encoded_bytes().len() >= 108 {
            return Err(LedgerError::NameTooLong {
                kind: "control socket path",
                name: socket.display().to_string(),
                limit: 107,
            });
        }
        let checkout = StoreLayout::project_fork(Path::new(&root.checkout_path), &request.name);
        let model = self
            .db
            .insert_fork(
                WorldActiveModel {
                    id: sea_orm::ActiveValue::NotSet,
                    checkout_path: sea_orm::ActiveValue::Set(path(&checkout)),
                    world_name: sea_orm::ActiveValue::Set(world_name.as_str().to_owned()),
                    store_root: sea_orm::ActiveValue::Set(root.store_root),
                    state: sea_orm::ActiveValue::Set(WorldState::Creating),
                    supervisor_pid: sea_orm::ActiveValue::NotSet,
                    pods_dir: sea_orm::ActiveValue::NotSet,
                    started_at_ms: sea_orm::ActiveValue::NotSet,
                    problem: sea_orm::ActiveValue::NotSet,
                    updated_at_ms: sea_orm::ActiveValue::Set(request.now_ms),
                },
                ForkInsert {
                    root_id: request.root_id,
                    parent_id: request.parent_id,
                    name: request.name.as_str().to_owned(),
                    branch: request.branch,
                    created_at_ms: request.now_ms,
                },
            )
            .map_err(|error| taken(error, world_name.as_str()))?;
        Ok(ForkRow {
            world_id: model.world_id,
            root_id: model.root_id,
            parent_id: model.parent_id,
            name: model.name,
            branch: model.branch,
        })
    }

    /// Records `live` plus supervisor, pods, and start time, clearing any old problem.
    pub fn publish_live(&self, id: i64, live: LiveFork, now_ms: i64) -> LedgerResult<()> {
        self.db.set_state(
            id,
            WorldPatch::state(WorldState::Live)
                .supervisor_pid(live.supervisor_pid)
                .pods_dir(path(&live.pods_dir))
                .started_at_ms(live.started_at_ms)
                .clear_problem(),
            now_ms,
        )
    }

    /// Marks `stopping` while the supervisor reaps its pods.
    pub fn mark_stopping(&self, id: i64, now_ms: i64) -> LedgerResult<()> {
        self.db
            .set_state(id, WorldPatch::state(WorldState::Stopping), now_ms)
    }

    /// Marks `stopped`, forgetting the supervisor, pods, and any problem.
    pub fn mark_stopped(&self, id: i64, now_ms: i64) -> LedgerResult<()> {
        self.db.set_state(
            id,
            WorldPatch::state(WorldState::Stopped)
                .clear_supervisor()
                .clear_pods()
                .clear_problem(),
            now_ms,
        )
    }

    /// Marks `deleting` before `drop` destroys snapshots.
    pub fn mark_deleting(&self, id: i64, now_ms: i64) -> LedgerResult<()> {
        self.db.set_state(
            id,
            WorldPatch::state(WorldState::Deleting)
                .clear_supervisor()
                .clear_pods(),
            now_ms,
        )
    }

    /// Marks `broken` with the one-line divergence text, forgetting supervisor and pods.
    pub fn mark_broken(
        &self,
        id: i64,
        problem: impl Into<String>,
        now_ms: i64,
    ) -> LedgerResult<()> {
        self.db.set_state(
            id,
            WorldPatch::state(WorldState::Broken)
                .clear_supervisor()
                .clear_pods()
                .problem(problem),
            now_ms,
        )
    }

    /// Every root and fork row as plain data, ordered by id.
    pub fn list(&self) -> LedgerResult<LedgerRows> {
        let (worlds, forks) = self.db.list_rows()?;
        Ok(LedgerRows {
            worlds: worlds.into_iter().map(world_row).collect(),
            forks: forks
                .into_iter()
                .map(|fork| ForkRow {
                    world_id: fork.world_id,
                    root_id: fork.root_id,
                    parent_id: fork.parent_id,
                    name: fork.name,
                    branch: fork.branch,
                })
                .collect(),
        })
    }

    /// Resolves a target; an absent `state.db` yields `Ok(None)`.
    ///
    /// # Errors
    ///
    /// [`LedgerError::UnknownTarget`] / [`LedgerError::AmbiguousTarget`] fail closed.
    pub fn resolve(&self, scope: &ProjectScope, needle: &str) -> LedgerResult<Option<Resolved>> {
        if self.db.is_absent() {
            return Ok(None);
        }
        let rows = self.list()?;
        resolve_target(scope, &rows.worlds, &rows.forks, needle)
    }

    /// The single delete path: refuses roots and splices children in one transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::UnknownRow`] for an id the ledger never had,
    /// [`LedgerError::RootRow`] for a root row, and [`LedgerError::ReadOnly`] on a reader.
    pub fn delete_world_row(&self, id: i64) -> LedgerResult<()> {
        self.db.delete_fork_row(id)
    }
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn taken(error: LedgerError, world_name: &str) -> LedgerError {
    match error {
        LedgerError::Duplicate => LedgerError::NameTaken {
            world_name: world_name.to_owned(),
        },
        other => other,
    }
}

fn world_row(model: entity::WorldModel) -> WorldRow {
    WorldRow {
        id: model.id,
        checkout_path: PathBuf::from(model.checkout_path),
        world_name: model.world_name,
        store_root: PathBuf::from(model.store_root),
        state: model.state,
        supervisor_pid: model.supervisor_pid,
        pods_dir: model.pods_dir.map(PathBuf::from),
        started_at_ms: model.started_at_ms,
        problem: model.problem,
        updated_at_ms: model.updated_at_ms,
    }
}
