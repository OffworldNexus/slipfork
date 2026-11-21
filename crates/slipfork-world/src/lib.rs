//! Btrfs subvolumes, CoW snapshots, and the ioctls behind them.
//!
//! [`BtrfsEngine`] is the real adapter behind the [`WorldEngine`] port from
//! `specs/03-under-the-hood.md` §5. Every root is injected by the caller: this crate never reads
//! the environment, caches no global state, and never guesses a mount point.
//!
//! ```no_run
//! use std::path::PathBuf;
//!
//! use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};
//!
//! # fn main() -> slipfork_world::Result<()> {
//! let engine = BtrfsEngine::new(StoreLayout::new(PathBuf::from("/mnt/btrfs")));
//! engine.ensure_store()?;
//! engine.adopt_project(&PathBuf::from("/mnt/btrfs/projects/demo"))?;
//! # Ok(())
//! # }
//! ```

mod btrfs;
mod engine;
mod error;
mod layout;
mod port;

pub use btrfs::{is_btrfs_path, is_subvolume_path};
pub use engine::BtrfsEngine;
pub use error::{Error, NameKind, Result};
pub use layout::{ForkName, StoreLayout, WorldName};
pub use port::{Dirtiness, ForkPaths, WorldEngine};
