//! Btrfs subvolumes, CoW snapshots, per-world namespaces, and `crun` pods.
//!
//! [`BtrfsEngine`] is the real adapter behind the [`WorldEngine`] port from
//! `specs/03-under-the-hood.md` §5; [`WorldSupervisor`] owns one unprivileged
//! user/net/ipc/pid world namespace and launches every service as a `crun` pod inside it. Every
//! root is injected by the caller: this crate never reads the environment, caches no global state,
//! and never guesses a mount point.
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
pub mod control;
mod engine;
mod error;
mod layout;
pub mod namespaces;
mod pod;
mod port;
mod resources;
mod runtime_dir;
mod secure_file;
mod supervisor;

pub use btrfs::{is_btrfs_path, is_subvolume_path};
pub use control::{
    ControlError, InitFrame, PROTOCOL_VERSION, Request, decode_init_frame, decode_request, encode,
};
pub use engine::BtrfsEngine;
pub use error::{Error, NameKind, Result};
pub use layout::{ForkName, StoreLayout, WorldName};
pub use namespaces::{NAMESPACE_KINDS, NamespaceIdentity, NamespaceTable};
pub use pod::{Bind, BindSource, PodBundle, PodEnv, PodFile, PodHost, PodSpec, write_bundle};
pub use port::{Dirtiness, ForkPaths, WorldEngine};
pub use resources::{
    ResourceKind, S3Bucket, S3Credentials, S3Policy, SeaweedfsPlan, postgres, redis, seaweedfs,
};
pub use runtime_dir::RuntimePods;
pub use supervisor::{WorldHost, WorldSupervisor};
