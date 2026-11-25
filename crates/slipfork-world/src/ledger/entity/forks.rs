//! `forks`: flat lineage for every non-root world (`specs/03` §7.2).

use sea_orm::entity::prelude::*;

/// One fork row; its `world_id` is both the primary key and the `worlds.id` it describes.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "forks")]
pub struct Model {
    /// `worlds.id` of this fork (`ON DELETE CASCADE`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub world_id: i64,
    /// The root world this fork belongs to (`ON DELETE RESTRICT`).
    pub root_id: i64,
    /// The immediate parent world (`ON DELETE RESTRICT`); spliced on drop.
    pub parent_id: i64,
    /// The fork's `<root>/.forks/<name>` directory name.
    pub name: String,
    /// Git branch created for the fork, empty until the next `up`.
    pub branch: String,
    /// Epoch milliseconds.
    pub created_at_ms: i64,
}

/// Foreign keys live in `m0001_create_ledger`; no `sea-orm` relation is traversed.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
