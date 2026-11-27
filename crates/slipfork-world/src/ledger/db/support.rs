//! Caller-injected `state.db` paths plus the private runtime, `flock`, and active-model helpers.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use sea_orm::{ActiveValue, DatabaseConnection, DbErr, EntityTrait, QueryOrder};

use sea_orm_migration::prelude::MigratorTrait;
use sea_orm_migration::{SchemaManager, migrator::MigrationStatus, seaql_migrations};

use super::{Access, LEGACY_WORLDS_JSON, Migrator, STATE_FILE};
use crate::ledger::error::{LedgerError, LedgerResult};

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

pub(super) fn runtime() -> LedgerResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| LedgerError::io("start the ledger runtime", Path::new("tokio"), source))
}

#[allow(unsafe_code)]
pub(super) fn flock(lock: &File) -> LedgerResult<()> {
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

pub(super) fn unique_or_db(error: DbErr) -> LedgerError {
    if matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        LedgerError::Duplicate
    } else {
        LedgerError::db(error)
    }
}

pub(super) fn field<T: Into<sea_orm::Value>>(value: Option<T>) -> ActiveValue<T> {
    match value {
        Some(value) => ActiveValue::Set(value),
        None => ActiveValue::NotSet,
    }
}

pub(super) fn opt_field<T: Into<sea_orm::Value> + sea_orm::sea_query::Nullable>(
    value: Option<Option<T>>,
) -> ActiveValue<Option<T>> {
    match value {
        Some(value) => ActiveValue::Set(value),
        None => ActiveValue::NotSet,
    }
}

pub(super) async fn gate(conn: &DatabaseConnection, access: Access) -> LedgerResult<()> {
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

pub(super) async fn applied_versions(conn: &DatabaseConnection) -> LedgerResult<Vec<String>> {
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
