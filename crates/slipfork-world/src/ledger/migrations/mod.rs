//! Forward-only ledger migrations (`specs/03` §7.2): `up` only, never auto-wipe.

pub mod m0001_create_ledger;

use sea_orm_migration::prelude::*;

pub use m0001_create_ledger::M0001CreateLedger;

/// The shipped migration sequence.
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(M0001CreateLedger)]
    }
}
