//! `sea-orm` entities for the two ledger tables (`specs/03` §7.2).

pub mod forks;
pub mod worlds;

pub use forks::{
    ActiveModel as ForkActiveModel, Column as ForkColumn, Entity as Forks, Model as ForkModel,
};
pub use worlds::{
    ActiveModel as WorldActiveModel, Column as WorldColumn, Entity as Worlds, Model as WorldModel,
    WorldState,
};
