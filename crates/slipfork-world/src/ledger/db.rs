//! `state.db`: one private `current_thread` runtime, WAL pragmas, a caller-owned `flock`
//! descriptor, and the version gate that runs before `Migrator::up` (`specs/03` §7.1–§7.2, §7.6).
//!
//! Every public method is synchronous. `run` enters the private runtime exactly once per call —
//! directly outside async code, on a scoped thread when a tokio handle is current — so `async` and
//! `block_on` never escape this file.

use std::fmt;
use std::fs::{self, File, Permissions};
use std::future::Future;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sea_orm::sea_query::Expr;
use sea_orm::sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QueryOrder, SqlxSqliteConnector, TransactionTrait,
};
use sea_orm_migration::prelude::MigratorTrait;
use sea_orm_migration::{SchemaManager, migrator::MigrationStatus, seaql_migrations};

use super::ChainEntry;
use super::entity::{forks, worlds};
use super::error::{LedgerError, LedgerResult};
use super::migrations::Migrator;
use super::patch::WorldPatch;
use crate::secure_file::{ensure_private_dir, touch_private};

/// The SQLite file inside `<state_dir>`.
pub const STATE_FILE: &str = "state.db";
/// The stale pre-`1.0` registry `open_writer` removes (`specs/03` §7.6).
pub const LEGACY_WORLDS_JSON: &str = "worlds.json";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Caller-injected paths; the ledger never derives them from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    /// `<state_dir>` holding `state.db` and its WAL.
    pub state_dir: PathBuf,
}

impl StatePaths {
    /// Binds a state directory.
    #[must_use]
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
        }
    }

    /// `<state_dir>/state.db`.
    #[must_use]
    pub fn db_file(&self) -> PathBuf {
        self.state_dir.join(STATE_FILE)
    }

    /// `<state_dir>/worlds.json`, the pre-`1.0` registry.
    #[must_use]
    pub fn worlds_json(&self) -> PathBuf {
        self.state_dir.join(LEGACY_WORLDS_JSON)
    }
}

/// Whether a handle may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The single daemon writer.
    Writer,
    /// Offline `ps`/`status` readers.
    Reader,
}

/// The fork half of `begin_fork`; `root_id` is the family, `parent_id` the lineage edge.
#[derive(Debug, Clone)]
pub(crate) struct ForkInsert {
    pub root_id: i64,
    pub parent_id: i64,
    pub name: String,
    pub branch: String,
    pub created_at_ms: i64,
}

/// One `state.db` handle: the runtime, the pool, and the daemon's lock descriptor.
pub struct StateDb {
    runtime: Option<tokio::runtime::Runtime>,
    conn: Option<DatabaseConnection>,
    _lock: Option<File>,
    db_file: PathBuf,
    access: Access,
}

impl fmt::Debug for StateDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateDb")
            .field("db_file", &self.db_file)
            .field("access", &self.access)
            .field("open", &self.conn.is_some())
            .finish()
    }
}

impl Drop for StateDb {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let in_async = tokio::runtime::Handle::try_current().is_ok();
        let conn = self.conn.take();
        if in_async {
            drop(conn);
            runtime.shutdown_background();
        } else if let Some(conn) = conn {
            let _ = runtime.block_on(conn.close());
        }
    }
}

impl StateDb {
    /// Opens the single writer on the caller's already-open daemon lock descriptor.
    ///
    /// `flock(2)` is taken **before** `state.db` is created or chmodded; the same
    /// open file description re-flocking is a no-op success, while a second process's
    /// descriptor is refused with [`LedgerError::WriterBusy`].
    pub(crate) fn open_writer(paths: StatePaths, lock: File) -> LedgerResult<Self> {
        flock(&lock)?;
        ensure_private_dir(&paths.state_dir)?;
        let db_file = paths.db_file();
        touch_private(&db_file)?;
        fs::set_permissions(&db_file, Permissions::from_mode(0o600))
            .map_err(|source| LedgerError::io("chmod", &db_file, source))?;
        let options = SqliteConnectOptions::new()
            .filename(&db_file)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let mut db = Self {
            runtime: Some(runtime()?),
            conn: None,
            _lock: Some(lock),
            db_file,
            access: Access::Writer,
        };
        let conn = db.run(async move {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .map_err(LedgerError::db)?;
            let conn = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);
            gate(&conn, Access::Writer).await?;
            Migrator::up(&conn, None).await.map_err(LedgerError::db)?;
            Ok::<_, LedgerError>(conn)
        })?;
        db.conn = Some(conn);
        let _ = fs::remove_file(paths.worlds_json());
        Ok(db)
    }

    /// Opens a read-only handle; an absent `state.db` yields an empty, non-error reader.
    pub(crate) fn open_reader(state_dir: PathBuf) -> LedgerResult<Self> {
        let db_file = state_dir.join(STATE_FILE);
        let mut db = Self {
            runtime: Some(runtime()?),
            conn: None,
            _lock: None,
            db_file,
            access: Access::Reader,
        };
        if !db.db_file.exists() {
            return Ok(db);
        }
        let options = SqliteConnectOptions::new()
            .filename(&db.db_file)
            .read_only(true)
            .busy_timeout(BUSY_TIMEOUT);
        let conn = db.run(async move {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .map_err(LedgerError::db)?;
            let conn = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);
            gate(&conn, Access::Reader).await?;
            Ok::<_, LedgerError>(conn)
        })?;
        db.conn = Some(conn);
        Ok(db)
    }

    /// Runs one ledger future on the private runtime without nesting runtimes.
    fn run<F, T>(&self, future: F) -> LedgerResult<T>
    where
        F: Future<Output = LedgerResult<T>> + Send,
        T: Send,
    {
        let runtime = self
            .runtime
            .as_ref()
            .expect("the ledger runtime lives as long as the handle");
        if tokio::runtime::Handle::try_current().is_err() {
            return runtime.block_on(future);
        }
        std::thread::scope(|scope| {
            scope
                .spawn(|| runtime.block_on(future))
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    }

    /// Whether this handle may write.
    #[must_use]
    pub fn access(&self) -> Access {
        self.access
    }

    /// Whether the reader found no `state.db` at all.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        self.conn.is_none()
    }

    /// Every applied migration version, oldest first; empty for an absent database.
    pub fn applied_versions(&self) -> LedgerResult<Vec<String>> {
        let Some(conn) = &self.conn else {
            return Ok(Vec::new());
        };
        self.run(applied_versions(conn))
    }

    /// Every `worlds` and `forks` row, deterministically ordered.
    pub(crate) fn list_rows(&self) -> LedgerResult<(Vec<worlds::Model>, Vec<forks::Model>)> {
        let Some(conn) = &self.conn else {
            return Ok((Vec::new(), Vec::new()));
        };
        self.run(async {
            let worlds = worlds::Entity::find()
                .order_by_asc(worlds::Column::Id)
                .all(conn)
                .await
                .map_err(LedgerError::db)?;
            let forks = forks::Entity::find()
                .order_by_asc(forks::Column::WorldId)
                .all(conn)
                .await
                .map_err(LedgerError::db)?;
            Ok((worlds, forks))
        })
    }

    /// The family root and immediate parent, refusing a parent outside the root's family.
    pub(crate) fn family_rows(
        &self,
        root_id: i64,
        parent_id: i64,
    ) -> LedgerResult<(worlds::Model, worlds::Model)> {
        let conn = self.writer_conn()?;
        self.run(async move {
            let root = worlds::Entity::find_by_id(root_id)
                .one(conn)
                .await
                .map_err(LedgerError::db)?
                .ok_or(LedgerError::UnknownRow { id: root_id })?;
            if forks::Entity::find_by_id(root_id)
                .one(conn)
                .await
                .map_err(LedgerError::db)?
                .is_some()
            {
                return Err(LedgerError::ForeignParent { root_id, parent_id });
            }
            let parent = if parent_id == root_id {
                root.clone()
            } else {
                worlds::Entity::find_by_id(parent_id)
                    .one(conn)
                    .await
                    .map_err(LedgerError::db)?
                    .ok_or(LedgerError::UnknownRow { id: parent_id })?
            };
            if parent.store_root != root.store_root {
                return Err(LedgerError::ForeignParent { root_id, parent_id });
            }
            if parent_id != root_id {
                let fork = forks::Entity::find_by_id(parent_id)
                    .one(conn)
                    .await
                    .map_err(LedgerError::db)?;
                if !matches!(fork, Some(fork) if fork.root_id == root_id) {
                    return Err(LedgerError::ForeignParent { root_id, parent_id });
                }
            }
            Ok((root, parent))
        })
    }

    /// Inserts one `worlds` row and returns it with its assigned id.
    pub(crate) fn insert_world(&self, world: worlds::ActiveModel) -> LedgerResult<worlds::Model> {
        let conn = self.writer_conn()?;
        self.run(async { world.insert(conn).await.map_err(unique_or_db) })
    }

    /// Inserts `worlds(state=creating)` plus its `forks` row in one transaction.
    pub(crate) fn insert_fork(
        &self,
        world: worlds::ActiveModel,
        fork: ForkInsert,
    ) -> LedgerResult<forks::Model> {
        let conn = self.writer_conn()?;
        self.run(async {
            let tx = conn.begin().await.map_err(LedgerError::db)?;
            let world = world.insert(&tx).await.map_err(unique_or_db)?;
            let model = forks::ActiveModel {
                world_id: ActiveValue::Set(world.id),
                root_id: ActiveValue::Set(fork.root_id),
                parent_id: ActiveValue::Set(fork.parent_id),
                name: ActiveValue::Set(fork.name),
                branch: ActiveValue::Set(fork.branch),
                created_at_ms: ActiveValue::Set(fork.created_at_ms),
            }
            .insert(&tx)
            .await
            .map_err(unique_or_db)?;
            tx.commit().await.map_err(LedgerError::db)?;
            Ok(model)
        })
    }

    /// Applies `patch` to one `worlds` row; a missing row is a typed error.
    pub(crate) fn set_state(&self, id: i64, patch: WorldPatch, now_ms: i64) -> LedgerResult<()> {
        let conn = self.writer_conn()?;
        self.run(async {
            let model = worlds::ActiveModel {
                id: ActiveValue::Set(id),
                checkout_path: ActiveValue::NotSet,
                world_name: ActiveValue::NotSet,
                store_root: ActiveValue::NotSet,
                state: field(patch.state),
                supervisor_pid: opt_field(patch.supervisor_pid),
                pods_dir: opt_field(patch.pods_dir),
                started_at_ms: opt_field(patch.started_at_ms),
                problem: opt_field(patch.problem),
                updated_at_ms: ActiveValue::Set(now_ms),
            };
            match model.update(conn).await {
                Ok(_) => Ok(()),
                Err(DbErr::RecordNotUpdated) => Err(LedgerError::UnknownRow { id }),
                Err(error) => Err(LedgerError::db(error)),
            }
        })
    }

    /// The single delete path: splice children, then delete the `worlds` row (fork cascades).
    pub(crate) fn delete_fork_row(&self, id: i64) -> LedgerResult<()> {
        let conn = self.writer_conn()?;
        self.run(async {
            let tx = conn.begin().await.map_err(LedgerError::db)?;
            let Some(fork) = forks::Entity::find_by_id(id)
                .one(&tx)
                .await
                .map_err(LedgerError::db)?
            else {
                let world = worlds::Entity::find_by_id(id)
                    .one(&tx)
                    .await
                    .map_err(LedgerError::db)?;
                tx.rollback().await.map_err(LedgerError::db)?;
                return match world {
                    Some(world) => Err(LedgerError::RootRow {
                        world_name: world.world_name,
                    }),
                    None => Err(LedgerError::UnknownRow { id }),
                };
            };
            forks::Entity::update_many()
                .col_expr(forks::Column::ParentId, Expr::value(fork.parent_id))
                .filter(forks::Column::ParentId.eq(id))
                .exec(&tx)
                .await
                .map_err(LedgerError::db)?;
            worlds::Entity::delete_by_id(id)
                .exec(&tx)
                .await
                .map_err(LedgerError::db)?;
            tx.commit().await.map_err(LedgerError::db)?;
            Ok(())
        })
    }

    /// Inserts every missing world outermost-first, upserts each fork edge, and returns the
    /// innermost row. A fully current chain opens a transaction but performs zero writes.
    pub(crate) fn ensure_chain(
        &self,
        store_root: PathBuf,
        chain: &[ChainEntry],
        now_ms: i64,
    ) -> LedgerResult<worlds::Model> {
        let conn = self.writer_conn()?;
        self.run(async move {
            let tx = conn.begin().await.map_err(LedgerError::db)?;
            let store_root = store_root.to_string_lossy().into_owned();
            let mut ids: Vec<i64> = Vec::with_capacity(chain.len());
            let mut current = None;
            for (index, entry) in chain.iter().enumerate() {
                let checkout = entry.checkout_path.to_string_lossy().into_owned();
                let model = match worlds::Entity::find()
                    .filter(worlds::Column::CheckoutPath.eq(checkout.clone()))
                    .one(&tx)
                    .await
                    .map_err(LedgerError::db)?
                {
                    Some(model) => model,
                    None => worlds::ActiveModel {
                        id: ActiveValue::NotSet,
                        checkout_path: ActiveValue::Set(checkout),
                        world_name: ActiveValue::Set(entry.world_name.as_str().to_owned()),
                        store_root: ActiveValue::Set(store_root.clone()),
                        state: ActiveValue::Set(super::WorldState::Stopped),
                        supervisor_pid: ActiveValue::NotSet,
                        pods_dir: ActiveValue::NotSet,
                        started_at_ms: ActiveValue::NotSet,
                        problem: ActiveValue::NotSet,
                        updated_at_ms: ActiveValue::Set(now_ms),
                    }
                    .insert(&tx)
                    .await
                    .map_err(|error| match unique_or_db(error) {
                        LedgerError::Duplicate => LedgerError::NameTaken {
                            world_name: entry.world_name.as_str().to_owned(),
                        },
                        other => other,
                    })?,
                };
                if let Some(fork) = &entry.fork {
                    let Some(root_id) = ids.first().copied() else {
                        return Err(LedgerError::Invalid {
                            message: "a fork chain entry has no root world ahead of it\nnext: report this as a bug"
                                .to_owned(),
                        });
                    };
                    let Some(parent_id) = index.checked_sub(1).and_then(|prev| ids.get(prev).copied())
                    else {
                        return Err(LedgerError::Invalid {
                            message: "a fork chain entry has no parent world ahead of it\nnext: report this as a bug"
                                .to_owned(),
                        });
                    };
                    let name = fork.as_str().to_owned();
                    match forks::Entity::find_by_id(model.id)
                        .one(&tx)
                        .await
                        .map_err(LedgerError::db)?
                    {
                        Some(row) => {
                            let root_present = worlds::Entity::find_by_id(row.root_id)
                                .one(&tx)
                                .await
                                .map_err(LedgerError::db)?
                                .is_some();
                            let parent_present = worlds::Entity::find_by_id(row.parent_id)
                                .one(&tx)
                                .await
                                .map_err(LedgerError::db)?
                                .is_some();
                            // A live edge is never re-parented: a flat `<fork>--<root>` layout can
                            // only re-derive [root, child], so rewriting the topology would flatten
                            // real ancestry and lease origin. Only a dangling reference is healed.
                            let root_id = if root_present { row.root_id } else { root_id };
                            let parent_id = if parent_present {
                                row.parent_id
                            } else {
                                parent_id
                            };
                            if root_id != row.root_id
                                || parent_id != row.parent_id
                                || row.name != name
                            {
                                let mut active: forks::ActiveModel = row.into();
                                active.root_id = ActiveValue::Set(root_id);
                                active.parent_id = ActiveValue::Set(parent_id);
                                active.name = ActiveValue::Set(name);
                                active.update(&tx).await.map_err(unique_or_db)?;
                            }
                        }
                        None => {
                            forks::ActiveModel {
                                world_id: ActiveValue::Set(model.id),
                                root_id: ActiveValue::Set(root_id),
                                parent_id: ActiveValue::Set(parent_id),
                                name: ActiveValue::Set(name),
                                branch: ActiveValue::Set(String::new()),
                                created_at_ms: ActiveValue::Set(now_ms),
                            }
                            .insert(&tx)
                            .await
                            .map_err(unique_or_db)?;
                        }
                    }
                }
                ids.push(model.id);
                current = Some(model);
            }
            tx.commit().await.map_err(LedgerError::db)?;
            current.ok_or_else(|| LedgerError::Invalid {
                message: "a registration chain has no entries\nnext: report this as a bug".to_owned(),
            })
        })
    }

    fn writer_conn(&self) -> LedgerResult<&DatabaseConnection> {
        if self.access != Access::Writer {
            return Err(LedgerError::ReadOnly);
        }
        self.conn.as_ref().ok_or(LedgerError::ReadOnly)
    }
}

fn runtime() -> LedgerResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| LedgerError::io("start the ledger runtime", Path::new("tokio"), source))
}

#[allow(unsafe_code)]
fn flock(lock: &File) -> LedgerResult<()> {
    // SAFETY: `flock(2)` only consumes the live descriptor; it dereferences no memory.
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    if matches!(source.raw_os_error(), Some(libc::EWOULDBLOCK)) {
        Err(LedgerError::WriterBusy)
    } else {
        Err(LedgerError::io("lock", Path::new("<daemon lock>"), source))
    }
}

async fn gate(conn: &DatabaseConnection, access: Access) -> LedgerResult<()> {
    let applied = applied_versions(conn).await?;
    let known: Vec<String> = Migrator::migrations()
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    if let Some(unknown) = applied.iter().find(|version| !known.contains(version)) {
        return Err(LedgerError::SchemaNewer {
            version: unknown.clone(),
        });
    }
    if access == Access::Reader {
        let pending = Migrator::get_migration_with_status_read_only(conn)
            .await
            .map_err(LedgerError::db)?
            .into_iter()
            .filter(|migration| matches!(migration.status(), MigrationStatus::Pending))
            .count();
        if pending > 0 {
            return Err(LedgerError::PendingMigration { count: pending });
        }
    }
    Ok(())
}

async fn applied_versions(conn: &DatabaseConnection) -> LedgerResult<Vec<String>> {
    let exists = SchemaManager::new(conn)
        .has_table("seaql_migrations")
        .await
        .map_err(LedgerError::db)?;
    if !exists {
        return Ok(Vec::new());
    }
    let rows = seaql_migrations::Entity::find()
        .order_by_asc(seaql_migrations::Column::AppliedAt)
        .all(conn)
        .await
        .map_err(LedgerError::db)?;
    Ok(rows.into_iter().map(|row| row.version).collect())
}

fn unique_or_db(error: DbErr) -> LedgerError {
    if matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        LedgerError::Duplicate
    } else {
        LedgerError::db(error)
    }
}

fn field<T: Into<sea_orm::Value>>(value: Option<T>) -> ActiveValue<T> {
    match value {
        Some(value) => ActiveValue::Set(value),
        None => ActiveValue::NotSet,
    }
}

fn opt_field<T: Into<sea_orm::Value> + sea_orm::sea_query::Nullable>(
    value: Option<Option<T>>,
) -> ActiveValue<Option<T>> {
    match value {
        Some(value) => ActiveValue::Set(value),
        None => ActiveValue::NotSet,
    }
}
