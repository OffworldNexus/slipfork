//! `slipfork-engine`: the [`WorldEngine`] Dependency Injection port for copy-on-write worlds and
//! its single shipped implementation, [`BtrfsEngine`], over an injected `SubvolumeDriver` (one
//! unified Btrfs subvolume per world, `ADR-0010`).
//!
//! [`BtrfsEngine`] satisfies the one [`conformance::assert_world_engine_conformance`] contract: a
//! world is materialized from an immutable [`SnapshotId`] plus a multi-mount [`WorldSpec`], its
//! services are supervised and stopped before [`WorldEngine::freeze_world`] seals it, and it is
//! referenced only through the opaque in-process [`WorldHandle`] (owned by `slipforkd`, never
//! serialized, `H-010` #1).
//!
//! Read-only snapshot creation ([`WorldEngine::create_snapshot`], [`WorldEngine::freeze_world`])
//! and writable world cloning ([`WorldEngine::clone_world`]) are distinct signatures: there is no
//! snapshot-mode flag and no capability boolean to branch on (`ADR-0010` §3).

pub mod btrfs;
pub mod btrfs_engine;
pub mod child_env;
pub mod clock;
pub mod conformance;
pub mod env;
pub mod lineage;
pub mod linux_cow;
pub mod oci;
pub mod rootfs;
pub mod service;
pub mod world_init;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use btrfs_engine::BtrfsEngine;
pub use clock::{MonotonicClock, SystemClock, elapsed_millis};
pub use env::{ExecEnv, InheritedVar};
pub use lineage::{
    BaseLineage, BootLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage,
    ServiceKind, WorldLineage, WorldServiceInfo,
};
pub use linux_cow::current_uid_gid;
pub use rootfs::{BackingServiceKey, OciRootfs, WorldRootfs};
pub use service::ServiceLogs;

use slipfork_world::WorldTarget;

/// Single AppArmor/user-namespace remediation formatter (`ADR-0010` §4, §7).
///
/// Shared by [`BtrfsEngine`]'s world boot and the CLI's host probe: it never calls `current_exe()`,
/// so the composition root decides which binary directory the profile must name. The profile text
/// is the one `slipfork daemon unit --only apparmor` prints, so this hint never drifts from it: one
/// leading-slash brace expression naming every executable that performs `CLONE_NEWUSER` for a
/// world — the `bin_dir` binaries, host `/usr/bin/unshare`, and in-world `/.slipfork/bin/crun`.
pub fn userns_remediation_hint(bin_dir: &Path) -> String {
    let dir = bin_dir.display().to_string();
    let rel_bin_dir = dir.trim_matches('/');
    let bin_clause = if rel_bin_dir.is_empty() {
        "slipfork{,d,-world-init}".to_owned()
    } else {
        format!("{rel_bin_dir}/slipfork{{,d,-world-init}}")
    };
    format!(
        "unprivileged user namespaces are denied by AppArmor \
         (`kernel.apparmor_restrict_unprivileged_userns=1`); install the profile printed by \
         `slipfork daemon unit --only apparmor` at /etc/apparmor.d/slipfork \
         (`abi <abi/4.0>,` plus `profile slipfork \
         /{{{bin_clause},usr/bin/unshare,.slipfork/bin/crun}} flags=(unconfined) {{ userns, }}`), \
         then reload it with `sudo apparmor_parser -r /etc/apparmor.d/slipfork`"
    )
}

/// Stable filesystem label for a world target (fork/slot names are already validated DNS labels).
pub(crate) fn target_slug(target: &WorldTarget) -> String {
    match target {
        WorldTarget::Canonical => "canonical".to_owned(),
        WorldTarget::UserFork(fork) => format!("fork-{fork}"),
        WorldTarget::TestSlot(slot) => format!("slot-{slot}"),
    }
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

/// In-netns `$BASE_URL` loopback bridge (`H-011` #4 Variant a).
///
/// When a [`WorldSpec`] carries `Some(..)`, the engine's persistent world init binds
/// `127.0.0.1:<port>` **inside** the world network namespace and splices every accepted TCP
/// stream to `host_ingress_sock` — a filesystem-pathname `AF_UNIX` listener owned by
/// `slipforkd` (`<state_root>/http-ingress.sock`, `0600`). A pathname `AF_UNIX` socket lives in
/// the filesystem (not a network namespace), so it crosses `CLONE_NEWNET` without any relay in
/// the host namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIngressBridge {
    /// Port bound on `127.0.0.1` inside the world network namespace (`80` is allowed: the world
    /// user namespace grants `CAP_NET_BIND_SERVICE` and the world init sets
    /// `net.ipv4.ip_unprivileged_port_start=0`).
    pub port: u16,
    /// Pathname `AF_UNIX` socket served by `slipforkd`'s HTTP ingress.
    pub host_ingress_sock: PathBuf,
}

/// Everything needed to materialize and run one world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldSpec {
    /// World identity (`slipfork_world::WorldTarget`).
    pub target: WorldTarget,
    /// Dockerfile-derived `L0` rootfs, static `crun`, and backing-service OCI images
    /// (`ADR-0009` §1–§3).
    pub rootfs: WorldRootfs,
    /// Immutable base snapshot the world is cloned from.
    pub snapshot: SnapshotId,
    /// Repo worktree (`ReadWrite`) plus artifact volumes and seed dumps (`ReadOnly`).
    pub mounts: Vec<WorldMount>,
    /// Resolved world environment, including the world-scoped `HOME`.
    pub base_env: ExecEnv,
    /// `Some(..)` asks the engine to bridge `127.0.0.1:<port>` inside the world `netns`.
    pub local_ingress: Option<LocalIngressBridge>,
    /// Base-snapshot and seed facts the seeder measured for this world's boot.
    ///
    /// Recorded verbatim in the cloned world's [`WorldLineage`]; the engine never estimates them.
    pub boot: BootLineage,
}

impl WorldSpec {
    /// Fail closed on a spec no engine may materialize.
    pub fn validate(&self) -> Result<(), EngineError> {
        for path in [&self.rootfs.app_rootfs, &self.rootfs.crun_bin] {
            if !path.is_absolute() {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "rootfs path '{}' must be absolute (WorldRootfs provenance)",
                        path.display()
                    ),
                });
            }
        }
        for image in self.rootfs.service_images.values() {
            if !image.rootfs.is_absolute() || !image.config.is_absolute() {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "backing-service rootfs '{}' and config '{}' must be absolute",
                        image.rootfs.display(),
                        image.config.display()
                    ),
                });
            }
        }
        if self.base_env.home().is_none() {
            return Err(EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME (ExecEnv::world_scoped)"
                    .to_owned(),
            });
        }
        if let Some(ingress) = &self.local_ingress
            && !world_init::unix_socket_path_fits(&ingress.host_ingress_sock)
        {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "local_ingress.host_ingress_sock '{}' exceeds the {} byte AF_UNIX path limit",
                    ingress.host_ingress_sock.display(),
                    world_init::UNIX_SOCKET_PATH_MAX
                ),
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

/// In-process `(engine, seq)` lookup token owned inside `slipforkd` (never deserialized, never sent
/// over IPC, `H-010` #1): an engine honours any handle whose engine tag matches `self.name()` and
/// whose seq is live in its world table, failing closed with [`EngineError::WorldGone`] otherwise.
/// Its [`fmt::Debug`] is stable and exposes no filesystem, environment, or secret state.
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

/// One world-owned service (`ADR-0009` §2–§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Stable service id (`api`, `postgres`, `redis`).
    pub id: String,
    /// The caller's classification of this row (`snow.yml` component vs backing service).
    pub kind: ServiceKind,
    /// Exactly one runtime: an `L0` world process or a daemonless `crun` container.
    pub runtime: ServiceRuntime,
    /// Overlay environment applied on top of the world's base env.
    pub env: ExecEnv,
    /// Port that must accept connections before the service counts as ready.
    pub readiness_port: Option<u16>,
}

/// Where one supervised service executes (`ADR-0009` §3).
///
/// Both variants flow through the same `spawn_service` / `stop_service` / `service_logs`
/// surface — there is no second supervisor and no boolean mode flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRuntime {
    /// An `L0` app component running on the world rootfs as `$UID:$GID`.
    WorldProcess {
        /// Command and arguments.
        argv: Vec<String>,
        /// Working directory inside the world.
        cwd: PathBuf,
    },
    /// A daemonless `crun` backing-service container (`ADR-0009` §2).
    ///
    /// The image rootfs is read-only, the container shares the world's `127.0.0.1` (no network
    /// namespace), and every process runs as the invoking host `$UID:$GID`.
    OciContainer {
        /// `snow.yml` `resources:` key resolved to an [`OciRootfs`] in [`WorldRootfs`].
        image: BackingServiceKey,
        /// Container command and arguments.
        argv: Vec<String>,
        /// Read-write CoW state under `<world_dir>/state`.
        state_mounts: Vec<StateSubdirMount>,
        /// Read-only host mounts (for example the verified `<sha256>.pgdump`, `ADR-0008` §2).
        readonly_mounts: Vec<WorldMount>,
    },
}

/// One read-write CoW state directory handed to a [`ServiceRuntime::OciContainer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubdirMount {
    /// Directory relative to the world's CoW `state_dir` (`pgdata/main`, `redis/cache`, `s3`).
    pub host_subdir: PathBuf,
    /// Absolute mount destination inside the container (`/var/lib/postgresql/data`, `/data`).
    pub container_path: PathBuf,
}

/// Lifecycle state of one world-owned service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// The service process is alive.
    Running,
    /// The service process exited on its own (crash or clean exit); it is never restarted.
    Exited,
    /// [`WorldEngine::stop_service`] (or a parent world teardown/freeze) stopped it.
    Stopped,
}

impl ServiceState {
    /// The bit representation an out-of-crate [`WorldEngine`] adapter stores in the supervision
    /// cell it mints [`ServiceHandle`]s over.
    pub fn to_bits(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Exited => 1,
            Self::Stopped => 2,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Self::Running,
            1 => Self::Exited,
            _ => Self::Stopped,
        }
    }
}

/// Opaque handle to one supervised service inside a live world.
///
/// Minted only by an engine. Its [`fmt::Debug`] exposes the service id and lifecycle state —
/// never a filesystem path, environment value, or process id.
#[derive(Clone)]
pub struct ServiceHandle {
    world: WorldHandle,
    id: String,
    state: Arc<AtomicU8>,
}

impl ServiceHandle {
    pub(crate) fn mint(world: WorldHandle, id: impl Into<String>, state: Arc<AtomicU8>) -> Self {
        Self {
            world,
            id: id.into(),
            state,
        }
    }

    /// World the service belongs to.
    pub fn world(&self) -> &WorldHandle {
        &self.world
    }

    /// Stable service id (`api`, `postgres`, `redis`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Current lifecycle state (never silently restarted after `Exited`).
    pub fn state(&self) -> ServiceState {
        ServiceState::from_bits(self.state.load(Ordering::Relaxed))
    }
}

impl fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceHandle")
            .field("world", &self.world)
            .field("id", &self.id)
            .field("state", &self.state())
            .finish()
    }
}

/// Engine-supplied argv prefix for entering a live world inheriting the caller's native stdio.
///
/// The thin `slipfork` CLI spawns `program` with `args_prefix` followed by the target argv,
/// `cwd` as its working directory, and native `Stdio::inherit()` — zero `SCM_RIGHTS` descriptors
/// cross the CLI <-> `slipforkd` boundary (`H-010` #1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntryCommand {
    /// Executable of the engine's entry program (for example `nsenter`).
    pub program: PathBuf,
    /// Arguments that must precede the target argv.
    ///
    /// The entry program starts the target in a directory that exists **inside** the world: a
    /// host pathname is unreachable after the world's `pivot_root`, so the entry shell's first
    /// `getcwd(2)` would fail there.
    pub args_prefix: Vec<String>,
    /// Working directory of the process that spawns [`Self::program`].
    ///
    /// `/` on purpose: it is the one directory that exists both on the host and inside every
    /// world rootfs, so the spawner can `chdir` before `exec`ing the entry program without
    /// touching a path the world owns.
    pub cwd: PathBuf,
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

/// One attributed chunk of a **retained** component/hook log.
///
/// [`StreamChunk`] is the un-attributed stream a live exec or hook run yields; a retained log chunk
/// also names the component or hook id it came from, so `slipfork logs` can attribute interleaved
/// output from several sources without the renderer guessing which service produced which line.
/// The id is the engine's own record ([`ServiceLogs::target`]) for a service and the daemon's hook
/// record for a hook — never the caller's request string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    /// Component id (`api`) or hook id (`api:pre-deploy`) that produced the bytes.
    pub target: String,
    /// Originating stream.
    pub source: StreamSource,
    /// Raw bytes (not necessarily UTF-8).
    pub bytes: Vec<u8>,
}

impl LogChunk {
    /// Attribute one retained [`StreamChunk`] to the component/hook `target` that produced it.
    pub fn attribute(target: impl Into<String>, chunk: StreamChunk) -> Self {
        Self {
            target: target.into(),
            source: chunk.source,
            bytes: chunk.bytes,
        }
    }
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
    #[error(
        "{path} is not on Btrfs: statfs reports f_type {actual_fstype:#x}, not BTRFS_SUPER_MAGIC 0x9123683e; fix: {mount_hint}"
    )]
    BtrfsRequired {
        path: PathBuf,
        actual_fstype: i64,
        mount_hint: &'static str,
    },
    #[error(
        "cannot destroy subvolume {path}: permission denied (errno {errno}); unprivileged subvolume deletion requires: {mount_hint}"
    )]
    SubvolumeDestroyDenied {
        path: PathBuf,
        errno: i32,
        mount_hint: &'static str,
    },
    #[error("btrfs {op} failed at {path} (errno {errno})")]
    Ioctl {
        op: &'static str,
        path: PathBuf,
        errno: i32,
    },
    #[error("unprivileged user namespace denied: {detail}; fix: {hint}")]
    UsernsDenied { detail: String, hint: String },
    #[error("snapshot '{0}' does not exist")]
    SnapshotNotFound(String),
    #[error(
        "snapshot '{id}' already exists and a sealed snapshot is immutable; destroy it explicitly \
         before sealing a new snapshot under the same id"
    )]
    SnapshotExists { id: String },
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
    #[error("world init for {world} failed: {reason}")]
    WorldInitFailed { world: String, reason: String },
    #[error("world {world} already runs a live service '{id}'")]
    ServiceAlreadyRunning { world: String, id: String },
    #[error("world {world} has no supervised service '{id}'")]
    UnknownService { world: String, id: String },
    #[error(
        "world {world} service '{id}' is not running (ServiceNotRunning); start the backing service before executing inside it"
    )]
    ServiceNotRunning { world: String, id: String },
    #[error("cannot dial 127.0.0.1:{port} inside world {world} (errno {errno})")]
    DialFailed {
        world: String,
        port: u16,
        errno: i32,
    },
    #[error("engine '{engine}' does not support {capability}")]
    Unsupported {
        engine: &'static str,
        capability: &'static str,
    },
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
    /// Short engine identifier used in diagnostics and world-handle ownership checks.
    fn name(&self) -> &'static str;

    /// `Self: Sized` excludes `dyn WorldEngine` so dynamic-port consumers obtain handles only from
    /// [`WorldEngine::clone_world`], while out-of-crate adapters and test doubles can construct
    /// their return token. Minting is an in-process lookup-token constructor, not an unforgeable
    /// capability: minting a live seq exercises full authority on any engine with a matching
    /// `name()`, and grants nothing across distinct engine tags or over IPC.
    fn mint_world_handle(&self, seq: u64) -> WorldHandle
    where
        Self: Sized,
    {
        WorldHandle::mint(self.name(), seq)
    }

    /// Mint the handle an out-of-crate adapter returns from [`WorldEngine::spawn_service`].
    ///
    /// `state` is the adapter's **own** supervision cell: the adapter stores
    /// [`ServiceState::Exited`] / [`ServiceState::Stopped`] when it reaps the service, and
    /// [`ServiceHandle::state`] reads that cell back. `Self: Sized` excludes `dyn WorldEngine`,
    /// mirroring [`WorldEngine::mint_world_handle`]: minting is an in-process lookup token, not an
    /// unforgeable capability, and grants nothing across engines or over IPC.
    fn mint_service_handle(
        &self,
        world: &WorldHandle,
        id: &str,
        state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> ServiceHandle
    where
        Self: Sized,
    {
        ServiceHandle::mint(world.clone(), id, state)
    }

    /// Capture an immutable read-only snapshot of `source_dir` under `id`.
    fn create_snapshot(
        &self,
        source_dir: &std::path::Path,
        id: &SnapshotId,
    ) -> Result<(), EngineError>;

    /// Whether the sealed snapshot `id` already exists on this engine's own substrate.
    ///
    /// A read-only lookup: it never creates, seals, or clones anything. The daemon uses it to skip
    /// a seeding pipeline whose only possible outcome is "the base already exists".
    fn snapshot_exists(&self, id: &SnapshotId) -> Result<bool, EngineError>;

    /// Seal the live world `world` into the immutable read-only snapshot `id`.
    ///
    /// Every live world-owned service is stopped cleanly **before** the world's `state_dir` is
    /// sealed, so the snapshot is crash-consistent (`H-011` #2). Freezing an already-frozen world
    /// fails closed with [`EngineError::WorldNotFreezable`]; a destroyed world is gone and fails
    /// closed with [`EngineError::WorldGone`]. Clones of the sealed snapshot are independent:
    /// writes in one clone are never visible to a sibling clone.
    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError>;

    /// Instantaneously clone `spec.snapshot` into a fresh isolated world.
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;

    /// Start `spec` as a supervised world-owned service.
    ///
    /// The engine captures its stdout/stderr into a size-capped retained [`ServiceLogs`] readable
    /// through [`WorldEngine::service_logs`], and — when `spec.readiness_port` is set — only
    /// returns once that port accepts connections inside the world. A service that exits is never
    /// silently restarted.
    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError>;

    /// Stop `service` with a bounded grace period.
    ///
    /// Idempotent: stopping an already-stopped or self-exited service succeeds, and the captured
    /// logs stay readable afterwards.
    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError>;

    /// Stop every service this engine supervises in `world`.
    ///
    /// The engine's own supervision record is the single owner of "what this world runs", so a
    /// caller that wants a world's services stopped (a `restart`, a rescue) asks here instead of
    /// replaying the handles it happens to hold: a partial spawn failure can never leave a running
    /// service behind. Services are stopped in `ADR-0009` §3 order — app components first, then
    /// backing-service containers — with the engine's bounded `SIGTERM` → `SIGKILL` grace.
    ///
    /// Idempotent: already-stopped and self-exited services stay stopped and keep their retained
    /// logs, and rows remain readable through [`WorldEngine::world_processes`]. A destroyed,
    /// foreign, or teardown-in-progress `world` fails closed with [`EngineError::WorldGone`] —
    /// never a silent no-op.
    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError>;

    /// Retained (size-capped) output of the service `id` inside `world`.
    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError>;

    /// Every service this engine supervises in `world`, ordered by service id.
    ///
    /// Rows come from the engine's own supervision records (`ServiceProcess` /
    /// `ContainerProcess`), never from a caller-side list: a service that exited or was stopped
    /// **keeps its row** with [`ServiceState::Exited`] / [`ServiceState::Stopped`] and `pid: None`,
    /// and two worlds never share a row. `uptime_secs` is stamped from the engine's injected
    /// [`MonotonicClock`] (zero `Instant` on the wire, zero second clock in the daemon); a service
    /// whose serving port the engine does not know reports `port: None` rather than `0`.
    ///
    /// A destroyed, teardown-in-progress, or foreign `world` fails closed with
    /// [`EngineError::WorldGone`] — never an empty row set.
    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError>;

    /// This world's own [`WorldLineage`], recorded on its handle when it booted.
    ///
    /// Rootfs and per-service image facts are the materialized [`WorldRootfs`] the clone was
    /// handed (cache outcome and `millis` measured on the same injected clock); base/seed facts
    /// are the [`BootLineage`] the seeder measured. A fact the engine did not record is `None` —
    /// there is no placeholder lineage and no daemon-wide "latest build" cell, so one world can
    /// never report another world's facts.
    ///
    /// A destroyed, teardown-in-progress, or foreign `world` fails closed with
    /// [`EngineError::WorldGone`] — never an all-`None` lineage.
    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError>;

    /// Replace a supervised service's overlay environment and restart it with its retained
    /// `ServiceSpec` (`M3.29` live `env_updated`).
    ///
    /// The caller owns the new environment (already decrypted and locally substituted in memory);
    /// the engine owns the argv, cwd, mounts, and readiness contract it captured at spawn time, so
    /// `DaemonService` never re-authors a `ServiceSpec`. Only this one service is stopped and
    /// respawned; sibling services are untouched. A read-only test adapter that cannot restart
    /// services reports [`EngineError::Unsupported`] rather than pretending it applied the update.
    fn update_service_env(
        &self,
        _service: &ServiceHandle,
        _env: &ExecEnv,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "update_service_env",
        })
    }

    /// Execute `argv` **inside a running service's own rootfs** — never the app rootfs — so
    /// `pg_restore`, `CHECKPOINT;`, and `psql` match the backing-service image (`ADR-0008` §2).
    ///
    /// Fails closed with [`EngineError::ServiceNotRunning`] unless the service is
    /// [`ServiceState::Running`].
    fn exec_in_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;

    /// Execute `argv` inside a running service with `input` streamed to its stdin.
    ///
    /// This is the one dump-restore seam: the decrypted `OsppSession::dump` stream is piped into
    /// `zstd -d | pg_restore` inside the container without materializing plaintext on disk. A
    /// read error from `input` is never swallowed (a truncated sealed dump must fail the restore
    /// even when the child happened to exit `0`). Adapters that cannot stream stdin report
    /// [`EngineError::Unsupported`].
    fn exec_in_service_with_input(
        &self,
        _service: &ServiceHandle,
        _argv: &[String],
        _input: Box<dyn std::io::Read + Send>,
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "exec_in_service_with_input",
        })
    }

    /// `crun exec` entry command for interactive service clients (`slipfork db` -> `psql`,
    /// `slipfork redis` -> `redis-cli`, `ADR-0009` §3).
    ///
    /// Fails closed with [`EngineError::ServiceNotRunning`] unless the service is
    /// [`ServiceState::Running`].
    fn service_entry_command(
        &self,
        service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError>;

    /// Execute `argv` inside `world` under the effective environment
    /// (`world.base_env ⊕ overlay_env`), streaming interleaved output chunks to `on_output`.
    ///
    /// `cwd` is the caller's **host** directory: the engine maps it through the world's bind
    /// mounts and starts `argv` on that in-world path (the rootfs `/` when the directory is not
    /// mounted), so the child's first `getcwd(2)` succeeds.
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &std::path::Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;

    /// Argv prefix letting the thin CLI enter `world` directly with inherited stdio (`H-010` #1).
    ///
    /// `cwd` is the caller's **host** directory; the returned `args_prefix` starts the target on
    /// that directory's in-world mapping (or the world rootfs `/`), never on the host pathname.
    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &std::path::Path,
    ) -> Result<WorldEntryCommand, EngineError>;

    /// Open a TCP stream to `127.0.0.1:<port>` **inside** `world`'s network namespace.
    ///
    /// Daemon-internal only (`slipforkd` HTTP/WS ingress and daemon-owned `forward`): the stream
    /// never crosses IPC. Dialing a destroyed world fails closed with [`EngineError::WorldGone`].
    fn dial_world_tcp(
        &self,
        world: &WorldHandle,
        port: u16,
    ) -> Result<std::net::TcpStream, EngineError>;

    /// Destroy `world`, reap its world init, and release its CoW state immediately.
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::userns_remediation_hint;

    #[test]
    fn the_userns_hint_names_every_clone_newuser_executable_and_the_reload() {
        let hint = userns_remediation_hint(Path::new("/usr/local/bin"));
        for needle in [
            "/{usr/local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}",
            "slipfork daemon unit --only apparmor",
            "apparmor_parser -r /etc/apparmor.d/slipfork",
        ] {
            assert!(hint.contains(needle), "hint missing {needle}: {hint}");
        }
        assert!(!hint.contains("//"), "{hint}");
    }

    #[test]
    fn the_userns_hint_renders_a_bare_clause_for_the_root_bin_dir() {
        let hint = userns_remediation_hint(Path::new("/"));
        assert!(
            hint.contains("/{slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}"),
            "{hint}"
        );
        assert!(!hint.contains("//"), "{hint}");
    }
}
