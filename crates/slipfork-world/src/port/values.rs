//! Plain world-engine values: specs, handles, lineage carriers, and fail-closed errors.
//!
//! No I/O, no process spawn, no ambient environment, no global.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pod::ServicePodSpec;
use crate::rootfs::{BackingServiceKey, WorldRootfs};
use crate::socket::{UNIX_SOCKET_PATH_MAX, unix_socket_path_fits};
use crate::{BaseLineage, ExecEnv, SeedLineage, ServiceKind, ServiceState, WorldTarget};

/// The base-snapshot and seed facts a world boots from, recorded on that world's handle.
///
/// `WorldEngine::clone_world` copies them verbatim into the cloned world's [`crate::WorldLineage`];
/// the seeder fills them: `base.millis` is `Some(ms)` when it sealed the base for this world and
/// `None` when the sealed snapshot already existed, and `seed.rebuilt` is `Some(true)` when it
/// restored a dump into the base, `Some(false)` when it reused an already-sealed base, and `None`
/// when the world ships no seed dump at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootLineage {
    /// Base snapshot key (full 64-hex) and its seal duration.
    pub base: Option<BaseLineage>,
    /// Seed dump digest, rebuild verdict, and measured duration.
    pub seed: Option<SeedLineage>,
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
    /// Recorded verbatim in the cloned world's [`crate::WorldLineage`]; the engine never estimates them.
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
            && !unix_socket_path_fits(&ingress.host_ingress_sock)
        {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "local_ingress.host_ingress_sock '{}' exceeds the {} byte AF_UNIX path limit",
                    ingress.host_ingress_sock.display(),
                    UNIX_SOCKET_PATH_MAX
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
    /// Mint the in-process lookup token an engine adapter returns (`slipfork-engine` seam).
    pub fn mint(engine: &'static str, seq: u64) -> Self {
        Self { engine, seq }
    }

    /// The engine-local sequence this token names.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// `true` when this token was minted by the engine tagged `engine`.
    pub fn belongs_to(&self, engine: &'static str) -> bool {
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
    /// The caller's classification of this row (`Forkfile` service vs backing service).
    pub kind: ServiceKind,
    /// Exactly one runtime: an `L0` world process, a daemonless `crun` backing-service container,
    /// or a `Forkfile` service pod (all three flow through [`WorldEngine::spawn_service`](crate::WorldEngine::spawn_service)).
    pub runtime: ServiceRuntime,
    /// `WorldProcess`: overlay applied on top of `WorldSpec::base_env`. `OciContainer`: the
    /// container's **complete declared** environment — a container bundle `config.json` never
    /// carries the world component env. `ServicePod`: stays empty, because the pod spec's own
    /// [`PodCommand`](crate::pod::PodCommand) carries the container's complete environment.
    pub env: ExecEnv,
    /// Port that must accept connections before the service counts as ready.
    pub readiness_port: Option<u16>,
}

/// Where one supervised service executes (`ADR-0009` §3, `ARCHITECTURE.md` §3.3).
///
/// Every variant flows through the same `spawn_service` / `stop_service` / `service_logs`
/// surface — there is no second supervisor and no boolean mode flag. An engine that has no pod
/// spawn path yet fails a [`ServiceRuntime::ServicePod`] spec closed with
/// [`EngineError::Unsupported`]; it never approximates the pod with another runtime.
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
        /// `Forkfile` resource key resolved to an [`crate::rootfs::OciRootfs`] in [`WorldRootfs`].
        image: BackingServiceKey,
        /// Container command and arguments.
        argv: Vec<String>,
        /// Read-write CoW state under `<world_dir>/state`.
        state_mounts: Vec<StateSubdirMount>,
        /// Read-only host mounts (for example the verified `<sha256>.pgdump`, `ADR-0008` §2).
        readonly_mounts: Vec<WorldMount>,
    },
    /// One `Forkfile` service as a daemonless `crun` container joined to the world's shared
    /// `slipfork-world-init` user/network/IPC namespaces (`ARCHITECTURE.md` §3.3).
    ///
    /// The pod spec owns everything the runtime decision needs — the OCI bundle directory under
    /// the engine control dir (`<ctl>/<world_id>/oci/<service>/`, never the world subvolume), the
    /// read-only rootfs, direct argv, complete environment, CoW volumes, and namespace joins — so
    /// the engine writes that bundle and never re-derives any of it. The joins come from
    /// [`crate::WorldEngine::world_pod_namespaces`]; this crate never probes `/proc` itself.
    ServicePod(ServicePodSpec),
}

/// One read-write CoW state directory handed to a [`ServiceRuntime::OciContainer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubdirMount {
    /// Directory relative to the world's CoW `state_dir` (`pgdata/main`, `redis/cache`, `s3`).
    pub host_subdir: PathBuf,
    /// Absolute mount destination inside the container (`/var/lib/postgresql/data`, `/data`).
    pub container_path: PathBuf,
}

/// Result of running [`crate::WorldEngine::exec_in_world`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Process exit code (`1` when the process died from a signal).
    pub exit_code: i32,
    /// Full, untruncated stdout.
    pub stdout: String,
    /// Full, untruncated stderr.
    pub stderr: String,
}

/// Fail-closed errors returned by [`crate::WorldEngine`] implementations (never silently degraded).
#[derive(Debug, Error)]
pub enum EngineError {
    #[error(
        "{path} is not on Btrfs: statfs reports f_type {actual_fstype:#x}, not BTRFS_SUPER_MAGIC 0x9123683e; fix: {mount_hint}"
    )]
    BtrfsRequired {
        path: PathBuf,
        actual_fstype: i64,
        mount_hint: String,
    },
    #[error(
        "cannot destroy subvolume {path}: permission denied (errno {errno}); unprivileged subvolume deletion requires: {mount_hint}"
    )]
    SubvolumeDestroyDenied {
        path: PathBuf,
        errno: i32,
        mount_hint: String,
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
    #[error("world {world} cannot be reloaded: it is sealed or being sealed")]
    WorldNotReloadable { world: String },
    #[error(
        "world {world} has no live world init: its last boot failed; reloading it again re-runs \
         the boot, and destroying it releases its storage"
    )]
    WorldNotLive { world: String },
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
    #[error(
        "cannot replace the environment of service '{id}' in world {world}: a container's bundle \
         env is fixed at spawn"
    )]
    ContainerEnvFixed { world: String, id: String },
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
    /// Mint the in-process service token an engine adapter returns.
    pub fn mint(world: WorldHandle, id: impl Into<String>, state: Arc<AtomicU8>) -> Self {
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

impl From<crate::EnvNameError> for EngineError {
    fn from(err: crate::EnvNameError) -> Self {
        Self::InvalidEnvName {
            name: err.name,
            reason: err.reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::pod::{PodCommand, PodLocation, PodNamespaces, VolumePlan};

    #[test]
    fn a_service_pod_runtime_carries_its_injected_spec_verbatim() {
        let world_subvol = Path::new("/var/tmp/slipfork/worlds/acme-1");
        let bundle_dir = Path::new("/var/tmp/slipfork/ctl/acme-1/oci/api");
        let rootfs = Path::new("/var/tmp/slipfork/cache/oci/sha256-abc/rootfs");
        let service_dir = Path::new("/home/dev/acme/api");
        let namespaces = PodNamespaces::new(
            Path::new("/proc/4242/ns/user"),
            Path::new("/proc/4242/ns/net"),
            Path::new("/proc/4242/ns/ipc"),
        )
        .expect("absolute namespace joins");
        let volumes = VolumePlan::new(
            world_subvol,
            "api",
            service_dir,
            &[".venv".to_owned(), "node_modules".to_owned()],
        )
        .expect("valid volume plan");
        let command = PodCommand::new(
            service_dir.to_path_buf(),
            vec!["uv".to_owned(), "run".to_owned(), "gunicorn".to_owned()],
            BTreeMap::from([("DATABASE_URL".to_owned(), "postgres://hunter2".to_owned())]),
        );
        let pod = ServicePodSpec::new(
            PodLocation::new(world_subvol, bundle_dir, rootfs).expect("valid locations"),
            namespaces.clone(),
            volumes.clone(),
            command,
        )
        .expect("valid pod");

        let spec = ServiceSpec {
            id: pod.service.clone(),
            kind: ServiceKind::Component,
            runtime: ServiceRuntime::ServicePod(pod.clone()),
            env: ExecEnv::world_scoped("/.slipfork/home"),
            readiness_port: Some(8000),
        };

        match &spec.runtime {
            ServiceRuntime::ServicePod(carried) => {
                assert_eq!(
                    carried, &pod,
                    "the port value carries the planned pod spec verbatim"
                );
                assert_eq!(carried.argv(), ["uv", "run", "gunicorn"]);
                assert_eq!(carried.cwd(), service_dir);
                assert_eq!(carried.bundle_dir, bundle_dir);
                assert_eq!(carried.rootfs, rootfs);
                assert_eq!(carried.volumes, volumes);
                assert_eq!(carried.namespace_joins(), namespaces.entries());
            }
            other => panic!("expected a service pod runtime, got {other:?}"),
        }

        let rendered = format!("{:?}", spec.runtime);
        assert!(rendered.contains("DATABASE_URL"), "{rendered}");
        assert!(
            !rendered.contains("hunter2"),
            "a pod runtime must never render environment values: {rendered}"
        );
    }
}
