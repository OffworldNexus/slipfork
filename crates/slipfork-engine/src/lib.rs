//! `slipfork-engine`: `WorldEngine` Dependency Injection port and concrete hypervisor/CoW drivers.
//!
//! Two production drivers implement [`WorldEngine`]:
//! - [`IncusEngine`]: talks to Incus `/1.0` REST API over Unix socket (LXC containers + QEMU VMs,
//!   ZFS/Btrfs/LVM-thin/XFS CoW, stateful RAM snapshots, `shift=true` host UID mounts).
//! - [`ReflinkNetnsEngine`]: clones stateful directories via Linux `ioctl(FICLONE)` on XFS/Btrfs
//!   and isolates execution inside unprivileged `CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWPID`
//!   namespaces preserving exact host `$UID:$GID`.

pub mod conformance;
pub mod incus;
pub mod linux_cow;
pub mod reflink_netns;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use incus::IncusEngine;
pub use linux_cow::{ReflinkStats, current_uid_gid, probe_reflink_dir, reflink_tree};
pub use reflink_netns::ReflinkNetnsEngine;

/// Honest capability vector advertised by a [`WorldEngine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineCapabilities {
    pub disk_cow: bool,
    pub stateful_ram_snapshot: bool,
    pub isolated_netns: bool,
    pub uid_passthrough: bool,
}

/// Content- or label-addressed snapshot handle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

/// Handle to a live cloned CoW world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldHandle {
    pub id: String,
    pub state_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

/// Host checkout / worktree directory mounted into the world under exact `$UID:$GID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBindMount {
    pub host_path: PathBuf,
}

/// Command specification executed inside a [`WorldHandle`].
#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// Result of running [`WorldEngine::exec_in_world`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Fail-closed errors returned by [`WorldEngine`] implementations (never silently degraded).
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("hypervisor daemon unavailable at {socket}: {reason}")]
    DaemonUnavailable { socket: String, reason: String },
    #[error(
        "filesystem at {path} does not support CoW FICLONE reflinks (errno {errno}); place state on XFS (reflink=1), Btrfs, or ZFS — never /tmp tmpfs"
    )]
    ReflinkUnsupported { path: String, errno: i32 },
    #[error("stateful RAM snapshots are not supported by engine '{engine}'")]
    StatefulRamUnsupported { engine: &'static str },
    #[error("snapshot '{0}' does not exist")]
    SnapshotNotFound(String),
    #[error("engine protocol/execution error: {0}")]
    Execution(String),
    #[error("engine I/O error at {path}: {message}")]
    Io { path: String, message: String },
}

/// Core Dependency Injection port for CoW world creation, cloning, execution, and teardown.
pub trait WorldEngine: Send + Sync {
    /// Short engine identifier (`"incus"` or `"reflink-netns"`).
    fn name(&self) -> &'static str;

    /// Honest capabilities supported by this engine.
    fn capabilities(&self) -> EngineCapabilities;

    /// Capture an immutable CoW snapshot from `source_state_dir`.
    fn create_snapshot(
        &self,
        source_state_dir: &std::path::Path,
        label: &str,
        stateful_ram: bool,
    ) -> Result<SnapshotId, EngineError>;

    /// Instantaneously clone `snapshot` into a fresh isolated [`WorldHandle`] mounting `bind`.
    fn clone_world(
        &self,
        snapshot: &SnapshotId,
        world_name: &str,
        bind: &HostBindMount,
    ) -> Result<WorldHandle, EngineError>;

    /// Execute `spec` inside `world`'s isolated network/process namespace under host `$UID:$GID`.
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        spec: &ExecSpec,
    ) -> Result<ExecOutput, EngineError>;

    /// Destroy `world` and release its CoW state immediately.
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
