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
//! let engine = BtrfsEngine::new(StoreLayout::at(PathBuf::from("/mnt/btrfs/.slipfork-store")));
//! engine.ensure_store()?;
//! engine.adopt_project(&PathBuf::from("/mnt/btrfs/projects/demo"))?;
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

pub mod boot;
#[allow(unsafe_code)]
mod btrfs;
pub mod control;
mod engine;
mod error;
#[allow(unsafe_code)]
pub mod fdpass;
mod layout;
pub mod namespaces;
mod pod;
pub mod pod_log;
mod port;
#[allow(unsafe_code)]
mod reap;
mod resources;
#[allow(unsafe_code)]
mod runtime_dir;
mod secure_file;
#[allow(unsafe_code)]
mod supervisor;

pub use boot::{
    BootEvent, BootFailure, BootKind, BootStep, FailedHook, HookPhase, ImageNode, ResourceNode,
    ServiceHealth, ServiceNode, StampStore, StepOutcome, StepStamp, WorldPlan, classify_service,
    run_plan, world_plan,
};
pub use btrfs::{is_btrfs_path, is_subvolume_path};
pub use control::Stream;
pub use control::{
    ControlError, FrameLines, InitFrame, OutputKind, PROTOCOL_VERSION, Request, decode_init_frame,
    decode_request, encode,
};
pub use engine::BtrfsEngine;
pub use error::{Error, NameKind, Result};
pub use fdpass::{PAYLOAD_CAP, recv_fd, send_fd};
pub use layout::{ForkName, STORE_DIR, StoreLayout, WorldName};
pub use namespaces::{NAMESPACE_KINDS, NamespaceIdentity, NamespaceTable};
pub use pod::{Bind, BindSource, PodBundle, PodEnv, PodFile, PodHost, PodSpec, write_bundle};
pub use pod_log::{CHUNK, DIVIDER_CLOSE, DIVIDER_OPEN, LogKind, LogLine, LogReader, LogSink};
pub use port::{Dirtiness, ForkPaths, WorldEngine};
pub use reap::kill_group;
pub use resources::{
    ImageFacts, ResourceKind, S3Bucket, S3Credentials, S3Policy, SeaweedfsPlan, postgres, redis,
    seaweedfs,
};
pub use runtime_dir::RuntimePods;
pub use supervisor::{PodStatus, WorldHost, WorldSupervisor, pod_statuses};
