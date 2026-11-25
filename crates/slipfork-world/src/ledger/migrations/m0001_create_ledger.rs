//! `worlds` and `forks`, exactly as `specs/03` §7.2 tabulates them.

use sea_orm_migration::prelude::*;

use crate::ledger::entity::{forks, worlds, worlds::WorldState};

/// Names the partial unique index over live-or-pending world names.
const ACTIVE_NAME_INDEX: &str = "idx_worlds_active_name";
/// Names the composite unique index matching `UNIQUE(store_root, world_name)`.
const STORE_NAME_INDEX: &str = "idx_worlds_store_name";
/// Every state the `CHECK` constraint accepts.
const ALL_STATES: [&str; 7] = [
    WorldState::Creating.as_str(),
    WorldState::Booting.as_str(),
    WorldState::Live.as_str(),
    WorldState::Stopping.as_str(),
    WorldState::Stopped.as_str(),
    WorldState::Deleting.as_str(),
    WorldState::Broken.as_str(),
];
/// Every state that still owns a world name (all but `broken`).
const ACTIVE_STATES: [&str; 6] = [
    WorldState::Creating.as_str(),
    WorldState::Booting.as_str(),
    WorldState::Live.as_str(),
    WorldState::Stopping.as_str(),
    WorldState::Stopped.as_str(),
    WorldState::Deleting.as_str(),
];

/// First ledger migration.
#[derive(DeriveMigrationName)]
pub struct M0001CreateLedger;

impl MigrationTrait for M0001CreateLedger {
    fn up<'life0, 'life1, 'async_trait>(
        &'life0 self,
        manager: &'life1 SchemaManager<'_>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), DbErr>> + Send + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
        'life1: 'async_trait,
    {
        Box::pin(async move {
            manager
                .create_table(
                    Table::create()
                        .table(worlds::Entity)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(worlds::Column::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(worlds::Column::CheckoutPath)
                                .string()
                                .not_null()
                                .unique_key(),
                        )
                        .col(
                            ColumnDef::new(worlds::Column::WorldName)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(worlds::Column::StoreRoot)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(worlds::Column::State)
                                .string()
                                .not_null()
                                .check(Expr::col(worlds::Column::State).is_in(ALL_STATES)),
                        )
                        .col(ColumnDef::new(worlds::Column::SupervisorPid).big_integer())
                        .col(ColumnDef::new(worlds::Column::PodsDir).string())
                        .col(ColumnDef::new(worlds::Column::StartedAtMs).big_integer())
                        .col(ColumnDef::new(worlds::Column::Problem).string())
                        .col(
                            ColumnDef::new(worlds::Column::UpdatedAtMs)
                                .big_integer()
                                .not_null(),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name(STORE_NAME_INDEX)
                        .if_not_exists()
                        .table(worlds::Entity)
                        .col(worlds::Column::StoreRoot)
                        .col(worlds::Column::WorldName)
                        .unique()
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name(ACTIVE_NAME_INDEX)
                        .if_not_exists()
                        .table(worlds::Entity)
                        .col(worlds::Column::StoreRoot)
                        .col(worlds::Column::WorldName)
                        .unique()
                        .and_where(Expr::col(worlds::Column::State).is_in(ACTIVE_STATES))
                        .to_owned(),
                )
                .await?;

            manager
                .create_table(
                    Table::create()
                        .table(forks::Entity)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(forks::Column::WorldId)
                                .big_integer()
                                .not_null()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(forks::Column::RootId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(forks::Column::ParentId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(ColumnDef::new(forks::Column::Name).string().not_null())
                        .col(
                            ColumnDef::new(forks::Column::Branch)
                                .string()
                                .not_null()
                                .default(""),
                        )
                        .col(
                            ColumnDef::new(forks::Column::CreatedAtMs)
                                .big_integer()
                                .not_null(),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_forks_world")
                                .from(forks::Entity, forks::Column::WorldId)
                                .to(worlds::Entity, worlds::Column::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_forks_root")
                                .from(forks::Entity, forks::Column::RootId)
                                .to(worlds::Entity, worlds::Column::Id)
                                .on_delete(ForeignKeyAction::Restrict),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_forks_parent")
                                .from(forks::Entity, forks::Column::ParentId)
                                .to(worlds::Entity, worlds::Column::Id)
                                .on_delete(ForeignKeyAction::Restrict),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_forks_root")
                        .if_not_exists()
                        .table(forks::Entity)
                        .col(forks::Column::RootId)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_forks_parent")
                        .if_not_exists()
                        .table(forks::Entity)
                        .col(forks::Column::ParentId)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_forks_root_name")
                        .if_not_exists()
                        .table(forks::Entity)
                        .col(forks::Column::RootId)
                        .col(forks::Column::Name)
                        .unique()
                        .to_owned(),
                )
                .await?;

            Ok(())
        })
    }
}
