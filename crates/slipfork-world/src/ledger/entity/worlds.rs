//! `worlds`: one row per root or fork world (`specs/03` §7.2).

use sea_orm::entity::prelude::*;

/// The lifecycle a world row records; `DEFAULT` is never assumed, every insert names one.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
pub enum WorldState {
    /// `begin_fork` inserted the rows, the snapshot has not landed yet.
    #[sea_orm(string_value = "creating")]
    Creating,
    /// A supervisor is booting this world's pods.
    #[sea_orm(string_value = "booting")]
    Booting,
    /// Supervisor ready and `Lease` written.
    #[sea_orm(string_value = "live")]
    Live,
    /// `down` asked the supervisor to stop every pod.
    #[sea_orm(string_value = "stopping")]
    Stopping,
    /// Pods reaped, checkout and world data preserved.
    #[sea_orm(string_value = "stopped")]
    Stopped,
    /// `drop` committed the fork to deletion.
    #[sea_orm(string_value = "deleting")]
    Deleting,
    /// The checkout vanished or the ledger and disk disagree.
    #[sea_orm(string_value = "broken")]
    Broken,
}

impl WorldState {
    /// The SQL string stored in `worlds.state`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Booting => "booting",
            Self::Live => "live",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Deleting => "deleting",
            Self::Broken => "broken",
        }
    }
}

/// One root or fork world.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "worlds")]
pub struct Model {
    /// Auto-incrementing row id; fork rows reference it.
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Absolute checkout path, unique across the store.
    #[sea_orm(unique)]
    pub checkout_path: String,
    /// `<basename>` for a root, `<fork>--<root>` for a fork.
    pub world_name: String,
    /// `<mount>/.slipfork-store`.
    pub store_root: String,
    /// Lifecycle state.
    pub state: WorldState,
    /// Supervisor process group leader, when live or booting.
    pub supervisor_pid: Option<i64>,
    /// `<runtime_dir>/slipfork/pods/<world>`.
    pub pods_dir: Option<String>,
    /// When the current boot published `live`.
    pub started_at_ms: Option<i64>,
    /// One-line convergence problem for a `broken` row.
    pub problem: Option<String>,
    /// Last mutation, epoch milliseconds.
    pub updated_at_ms: i64,
}

/// Foreign keys live in `m0001_create_ledger`; no `sea-orm` relation is traversed.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
