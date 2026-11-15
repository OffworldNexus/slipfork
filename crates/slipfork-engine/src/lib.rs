//! `slipfork-engine`: `WorldEngine` Dependency Injection port and concrete hypervisor/CoW drivers.
//!
//! Two production drivers implement [`WorldEngine`]:
//! - [`IncusEngine`]: talks to the Incus `/1.0` REST API over its Unix socket (LXC containers +
//!   QEMU VMs, ZFS/Btrfs/LVM-thin/XFS CoW, stateful RAM snapshots, `shift=true` host UID mounts).
//! - [`ReflinkNetnsEngine`]: clones stateful directories via Linux `ioctl(FICLONE)` on XFS/Btrfs
//!   and isolates execution inside unprivileged `CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWNET |
//!   CLONE_NEWPID` namespaces preserving the exact host `$UID:$GID`.
//!
//! Both drivers satisfy the one [`conformance::assert_world_engine_conformance`] contract: a
//! world is materialized from an immutable [`SnapshotId`] plus a multi-mount [`WorldSpec`], sealed
//! with [`WorldEngine::freeze_world`], and referenced only through the opaque in-process
//! [`WorldHandle`] (owned by `slipforkd`, never serialized, `H-010` #1).

pub mod child_env;
pub mod conformance;
pub mod env;
pub mod incus;
pub mod linux_cow;
pub mod reflink_netns;

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use env::{ExecEnv, InheritedVar};
pub use incus::IncusEngine;
pub use linux_cow::{ReflinkStats, current_uid_gid, probe_reflink_dir, reflink_tree};
pub use reflink_netns::ReflinkNetnsEngine;

use slipfork_world::WorldTarget;

/// Stable filesystem label for a world target (fork/slot names are already validated DNS labels).
pub(crate) fn target_slug(target: &WorldTarget) -> String {
    match target {
        WorldTarget::Canonical => "canonical".to_owned(),
        WorldTarget::UserFork(fork) => format!("fork-{fork}"),
        WorldTarget::TestSlot(slot) => format!("slot-{slot}"),
    }
}

/// Snapshot flavor requested from a [`WorldEngine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    /// Disk-only copy-on-write snapshot (reflink / hypervisor disk snapshot).
    DiskCow,
    /// Snapshot that also captures live process/VM memory.
    ///
    /// Engines that cannot capture memory state fail closed with
    /// [`EngineError::StatefulRamUnsupported`] — there is no capability boolean to branch on.
    StatefulRam,
}

/// Access mode of a [`WorldMount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    /// Read-only mount.
    ReadOnly,
    /// Read-write mount, owned by the host `$UID:$GID`.
    ReadWrite,
}

/// One host directory exposed to a world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldMount {
    /// Directory on the host.
    pub host_path: PathBuf,
    /// Path the directory is mounted at inside the world.
    pub guest_path: PathBuf,
    /// Read-only or read-write.
    pub mode: MountMode,
}

/// Content- or label-addressed snapshot handle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    /// Reject identifiers that are empty or could escape the engine's snapshot directory.
    pub fn validate(&self) -> Result<&str, EngineError> {
        if self.0.is_empty()
            || self.0 == "."
            || self.0 == ".."
            || !self
                .0
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(EngineError::InvalidSnapshotId { id: self.0.clone() });
        }
        Ok(&self.0)
    }
}

/// Everything needed to materialize and run one world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldSpec {
    /// World identity (`slipfork_world::WorldTarget`).
    pub target: WorldTarget,
    /// Immutable base snapshot the world is cloned from.
    pub snapshot: SnapshotId,
    /// Repo worktree (`ReadWrite`) plus artifact volumes and seed dumps (`ReadOnly`).
    pub mounts: Vec<WorldMount>,
    /// Resolved world environment, including the world-scoped `HOME`.
    pub base_env: ExecEnv,
    /// `Some(port)` asks the engine to bridge `127.0.0.1:<port>` inside the world `netns`.
    pub local_ingress_port: Option<u16>,
}

impl WorldSpec {
    /// Fail closed on a spec no engine may materialize.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.base_env.home().is_none() {
            return Err(EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME (ExecEnv::world_scoped)"
                    .to_owned(),
            });
        }
        for mount in &self.mounts {
            for path in [&mount.host_path, &mount.guest_path] {
                if !path.is_absolute() {
                    return Err(EngineError::InvalidWorldSpec {
                        reason: format!("mount path '{}' must be absolute", path.display()),
                    });
                }
            }
        }
        for outer in &self.mounts {
            if outer.mode != MountMode::ReadWrite {
                continue;
            }
            for inner in &self.mounts {
                if std::ptr::eq(outer, inner) || inner.mode != MountMode::ReadWrite {
                    continue;
                }
                if inner.host_path.starts_with(&outer.host_path)
                    || inner.guest_path.starts_with(&outer.guest_path)
                {
                    return Err(EngineError::OverlappingMounts {
                        outer: outer.host_path.display().to_string(),
                        inner: inner.host_path.display().to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Opaque in-process handle to one live world.
///
/// Minted exclusively by an engine (never by callers, never deserialized), owned exclusively
/// inside `slipforkd`, and never sent over IPC (`H-010` #1). Its [`fmt::Debug`] is stable and
/// exposes no filesystem, environment, or secret state.
#[derive(Clone, PartialEq, Eq)]
pub struct WorldHandle {
    engine: &'static str,
    seq: u64,
}

impl WorldHandle {
    pub(crate) fn mint(engine: &'static str, seq: u64) -> Self {
        Self { engine, seq }
    }

    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    pub(crate) fn belongs_to(&self, engine: &'static str) -> bool {
        self.engine == engine
    }
}

impl fmt::Debug for WorldHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WorldHandle({}#{})", self.engine, self.seq)
    }
}

/// One world-owned service (supervision lands in `M2.7`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Stable service id (`api`, `postgres`, `redis`).
    pub id: String,
    /// Command and arguments.
    pub argv: Vec<String>,
    /// Working directory inside the world.
    pub cwd: PathBuf,
    /// Overlay environment applied on top of the world's base env.
    pub env: ExecEnv,
    /// Port that must accept connections before the service counts as ready.
    pub readiness_port: Option<u16>,
}

/// Which child stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSource {
    /// Child stdout.
    Stdout,
    /// Child stderr.
    Stderr,
}

/// One streamed chunk of child output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Originating stream.
    pub source: StreamSource,
    /// Raw bytes (not necessarily UTF-8).
    pub bytes: Vec<u8>,
}

/// Result of running [`WorldEngine::exec_in_world`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Process exit code (`1` when the process died from a signal).
    pub exit_code: i32,
    /// Full, untruncated stdout.
    pub stdout: String,
    /// Full, untruncated stderr.
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
    #[error("invalid snapshot id '{id}': must be a plain 1..=128 byte alphanumeric/-/_/. label")]
    InvalidSnapshotId { id: String },
    #[error("invalid WorldSpec: {reason}")]
    InvalidWorldSpec { reason: String },
    #[error("invalid environment variable name '{name}': {reason}")]
    InvalidEnvName { name: String, reason: String },
    #[error("nested read-write mounts '{outer}' and '{inner}' would shadow each other")]
    OverlappingMounts { outer: String, inner: String },
    #[error("world {world} cannot be frozen: it is already sealed or destroyed")]
    WorldNotFreezable { world: String },
    #[error("world {world} is not owned by this engine (already destroyed or foreign)")]
    WorldGone { world: String },
    #[error("engine protocol/execution error: {0}")]
    Execution(String),
    #[error("engine I/O error at {path}: {message}")]
    Io { path: String, message: String },
}

/// Lifecycle state of a world inside one engine instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldStatus {
    Live,
    Freezing,
    Frozen,
    Destroyed,
}

/// Core Dependency Injection port for CoW world creation, cloning, execution, and teardown.
pub trait WorldEngine: Send + Sync {
    /// Short engine identifier (`"incus"` or `"reflink-netns"`).
    fn name(&self) -> &'static str;

    /// Capture an immutable snapshot of `source_dir` under `id`.
    fn create_snapshot(
        &self,
        source_dir: &std::path::Path,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError>;

    /// Seal the live world `world` into the immutable snapshot `id`.
    ///
    /// Freezing an already-frozen (or destroyed) world fails closed with
    /// [`EngineError::WorldNotFreezable`]. Clones of the sealed snapshot are independent: writes
    /// in one clone are never visible to a sibling clone.
    fn freeze_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError>;

    /// Instantaneously clone `spec.snapshot` into a fresh isolated world.
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;

    /// Execute `argv` inside `world` under the effective environment
    /// (`world.base_env ⊕ overlay_env`), streaming interleaved output chunks to `on_output`.
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &std::path::Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;

    /// Destroy `world` and release its CoW state immediately.
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
