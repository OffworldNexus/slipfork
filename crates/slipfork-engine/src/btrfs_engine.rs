//! `BtrfsEngine`: one unified Btrfs subvolume per world (`ADR-0010` §§2–3, `ADR-0002`, `H-011`).
//!
//! Every live world is exactly one writable Btrfs subvolume `<state_dir>/worlds/<world_id>`,
//! cloned from the read-only snapshot `<state_dir>/snapshots/<spec.snapshot>` with one
//! `BTRFS_IOC_SNAP_CREATE_V2` call, and sealed back into `<state_dir>/snapshots/<id>` with
//! `BTRFS_SUBVOL_RDONLY` through the concrete [`KernelBtrfsDriver`]. Inside that single subvolume
//! live the read-only `L0` rootfs reference (`<subvol>/rootfs`) and the backing-service state
//! (`<subvol>/state/{pgdata,redis,s3}`) as **plain directories** — never nested subvolumes, which
//! `BTRFS_IOC_SNAP_CREATE_V2` would not recurse into — so freeze, fork, and teardown are metadata
//! operations.
//!
//! Ephemeral runtime state — `netns.sock`, `world-init.json`, `init.ready`, `oci/`, `oci-run/`,
//! `mountpoints/`, `merged/`, and the synthesized `/etc/{passwd,group}` — lives in the per-world
//! control directory `<state_dir>/ctl/<world_id>` derived by this engine, **outside** the
//! subvolume, so a seal never captures a socket or `crun` runtime state (`ADR-0010` §2).
//!
//! Supervision is the persistent per-world PID 1 (`src/bin/slipfork-world-init.rs`) spawned inside
//! `unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork
//! --kill-child=SIGKILL --mount-proc` with a piped `stdin` lifeline. `--ipc` gives every world a
//! private System V IPC namespace, so parallel `postgres` containers never collide on `shmget` key
//! `90000` or port-derived keys (`ADR-0010` §6). No `unsafe` code lives here:
//! `statfs`/`BTRFS_IOC_*` are confined to [`crate::btrfs`] and `SCM_RIGHTS`/`kill(2)` to
//! [`crate::linux_cow`].

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::WorldTarget;

use crate::btrfs::KernelBtrfsDriver;
use crate::child_env;
use crate::linux_cow::{self, ProcessSignal};
use crate::oci::{self, ContainerBundle, ContainerMount};
use crate::service::{
    D_STATE_KILL_GRACE, POLL_INTERVAL, ProcessIdentity, ServiceLogs, container_id, drain_stderr,
    ensure_container_destination, host_child_pids, kill_and_reap_child, run_streamed,
    run_streamed_with_input, spawn_log_reader, wait_for_exit, wait_for_inner_pid, wait_reaped,
    world_guest_for_host,
};
use crate::world_init::{
    GROUP_GUEST, INGRESS_GUEST, OCI_BUNDLES_GUEST, OCI_RUN_GUEST, PASSWD_GUEST,
    WORLD_INIT_CONFIG_FILE, WORLD_INIT_SOCKET_FILE, WORLD_STATE_GUEST, WorldInitConfig,
    WorldInitIngress, WorldInitMount, WorldInitRootfs, image_guest_rootfs,
    synthesize_identity_files, unix_socket_path_fits, world_init_mount,
};
use crate::{
    BackingServiceKey, EngineError, ExecEnv, ExecOutput, MountMode, RootfsLineage, ServiceHandle,
    ServiceImageLineage, ServiceKind, ServiceRuntime, ServiceSpec, ServiceState, SnapshotId,
    StateSubdirMount, StreamChunk, WorldEngine, WorldEntryCommand, WorldHandle, WorldLineage,
    WorldMount, WorldRootfs, WorldServiceInfo, WorldSpec, WorldStatus, target_slug,
    userns_remediation_hint,
};

/// Installed file name of the per-world PID 1 helper (diagnostics only: the path is injected).
const WORLD_INIT_PROGRAM: &str = "slipfork-world-init";
/// Fixed search path for helper subprocesses (`unshare`, `nsenter`); nothing is inherited.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// Subdirectory of the world subvolume holding the read-only `L0` rootfs reference.
const WORLD_ROOTFS_DIR: &str = "rootfs";
/// Subdirectory of the world subvolume holding backing-service state.
const WORLD_STATE_DIR: &str = "state";
/// Category parents materialized inside `<subvol>/state`; concrete leaves (`pgdata/main`,
/// `redis/cache`, `s3/<bucket>`) are created by the owning service spec.
const WORLD_STATE_CATEGORIES: [&str; 3] = ["pgdata", "redis", "s3"];
/// Bound on a backing-service container accepting connections (images can be slow on first boot).
const OCI_SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on booting a world (mounts, loopback, listeners, `netns.sock`).
const WORLD_INIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on one `dial_world_tcp` round trip.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on waiting for `ServiceSpec::readiness_port` to accept connections.
const SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// Grace period between `SIGTERM` and `SIGKILL` when stopping a service.
const SERVICE_STOP_GRACE: Duration = Duration::from_secs(5);
/// Grace period for the world PID 1 to exit after closing its `stdin` lifeline / `SIGTERM`.
const WORLD_STOP_TERM_GRACE: Duration = Duration::from_secs(5);
/// Brief `stdin`-EOF window during [`Drop`] before escalating to `SIGKILL`.
const WORLD_DROP_TERM_GRACE: Duration = Duration::from_millis(100);
/// Grace period for the world `unshare` launcher and PID namespace to disappear after `SIGKILL`.
const WORLD_STOP_KILL_GRACE: Duration = Duration::from_secs(10);

/// The only Btrfs-backed [`WorldEngine`] (`ADR-0010` §1): one unified subvolume per world plus an
/// unprivileged `userns`/`netns`/`ipcns`/`pidns`/`mntns` PID 1. `slipforkd` wires it in `M3.15`.
pub struct BtrfsEngine {
    state_dir: PathBuf,
    host_uid: u32,
    host_gid: u32,
    world_init_bin: PathBuf,
    driver: KernelBtrfsDriver,
    shared: Arc<Shared>,
}

impl fmt::Debug for BtrfsEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsEngine")
            .field("state_dir", &self.state_dir)
            .field("host_uid", &self.host_uid)
            .field("host_gid", &self.host_gid)
            .field("world_init_bin", &self.world_init_bin)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Shared {
    worlds: Mutex<BTreeMap<u64, WorldRecord>>,
    next_seq: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            worlds: Mutex::new(BTreeMap::new()),
            next_seq: AtomicU64::new(0),
        }
    }
}

impl Drop for BtrfsEngine {
    fn drop(&mut self) {
        self.shared.worlds.clear_poison();
        let leaked: Vec<(u64, String, PathBuf, PathBuf)> = match self.lock() {
            Ok(worlds) => worlds
                .iter()
                .map(|(&seq, record)| {
                    (
                        seq,
                        record.world_id.clone(),
                        record.ctl_dir.clone(),
                        record.world_dir.clone(),
                    )
                })
                .collect(),
            Err(_) => return,
        };
        for (seq, world_id, ctl_dir, world_dir) in leaked {
            let handle = self.mint_world_handle(seq);
            match self.teardown_world(&handle, Duration::ZERO, WORLD_DROP_TERM_GRACE) {
                Ok(()) => {
                    eprintln!(
                        "slipfork-engine: leaked world '{world_id}' reclaimed on BtrfsEngine drop \
                         (ctl={}, subvolume={})",
                        ctl_dir.display(),
                        world_dir.display()
                    );
                }
                Err(err) => {
                    // Even if a service survived its stop bound, still reap the world launcher/init
                    // so no `unshare` supervisor outlives the dropped engine.
                    let _ = self.reap_world_init(&handle, Duration::ZERO);
                    eprintln!(
                        "slipfork-engine: failed to reclaim leaked world '{world_id}' on \
                         BtrfsEngine drop (ctl={}, subvolume={}): {err}",
                        ctl_dir.display(),
                        world_dir.display()
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorldSubvolume {
    /// `<state_dir>/worlds/<world_id>` exists on disk, cloned from this sealed snapshot.
    Cloned(SnapshotId),
    /// A base-changing reload destroyed the previous subvolume and has not cloned its replacement.
    Missing,
}

#[derive(Debug)]
struct WorldRecord {
    /// Subvolume name: `<state_dir>/worlds/<world_id>` and `<state_dir>/ctl/<world_id>`.
    world_id: String,
    /// The one writable world subvolume.
    world_dir: PathBuf,
    /// Ephemeral control directory, strictly outside the subvolume.
    ctl_dir: PathBuf,
    /// `<world_dir>/state`, bind-mounted read-write at `/.slipfork/state`.
    state_dir: PathBuf,
    /// `<ctl_dir>/netns.sock`, the `SCM_RIGHTS` dial socket.
    netns_sock: PathBuf,
    init_pid: u32,
    init_identity: Option<ProcessIdentity>,
    launcher: Option<Child>,
    base_env: ExecEnv,
    rootfs: WorldRootfs,
    /// On-disk base snapshot of this world's subvolume: a reload naming the same snapshot keeps
    /// the existing subvolume, while a different snapshot (or `Missing`) clones from `spec`.
    subvolume: WorldSubvolume,
    mounts: Vec<WorldMount>,
    status: WorldStatus,
    /// A teardown started and may still be running: every caller sees the world as gone, and
    /// [`WorldEngine::destroy_world`] re-enters where the previous attempt stopped.
    destroying: bool,
    services: BTreeMap<String, ServiceRecord>,
    /// This world's own boot lineage, recorded when its on-disk rootfs was materialized.
    lineage: WorldLineage,
}

impl WorldRecord {
    /// A world whose teardown has started or finished is gone for every caller.
    fn is_gone(&self) -> bool {
        self.destroying || self.status == WorldStatus::Destroyed
    }
}

/// Which spawn path produced a supervised service — the engine's private teardown/readiness
/// discriminator. The wire-facing classification is [`ServiceKind`] (component vs backing service).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeKind {
    WorldProcess,
    OciContainer,
}

#[derive(Debug)]
struct ServiceRecord {
    /// Which spawn path produced it (teardown/readiness discriminator).
    runtime_kind: RuntimeKind,
    /// The complete spawn contract retained for [`WorldEngine::update_service_env`]: argv, cwd,
    /// mounts, and readiness are the engine's to own so the daemon never re-authors a
    /// `ServiceSpec` to apply a new environment.
    spec: ServiceSpec,
    /// The caller-declared row classification (`ServiceSpec::kind`).
    kind: ServiceKind,
    /// In-world serving/readiness port the caller asked for, recorded at spawn.
    port: Option<u16>,
    /// Spawn instant of the service; `uptime_secs` is measured from it.
    started_at: Instant,
    log: ServiceLogs,
    state: Arc<AtomicU8>,
    process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
struct ServiceProcess {
    supervisor: Option<Child>,
    inner: Option<ProcessIdentity>,
    /// The group this service's launcher leads; stop signals the whole group so a descendant that
    /// outlived the supervised child is still reaped.
    process_group: Option<ProcessIdentity>,
    container: Option<ContainerProcess>,
}

/// One service's teardown handles, taken out of the world record before `destroy_world` releases
/// the world's processes and storage.
struct ServiceTeardown {
    runtime_kind: RuntimeKind,
    id: String,
    state: Arc<AtomicU8>,
    log: ServiceLogs,
    process: Arc<Mutex<ServiceProcess>>,
}

impl ServiceTeardown {
    fn from_record(id: &str, service: &ServiceRecord) -> Self {
        Self {
            runtime_kind: service.runtime_kind,
            id: id.to_owned(),
            state: Arc::clone(&service.state),
            log: service.log.clone(),
            process: Arc::clone(&service.process),
        }
    }
}

/// Validated, mutation-free pre-flight inputs resolved before [`WorldEngine::clone_world`] or
/// [`WorldEngine::reload_world`] touches a service, PID 1, control directory, or subvolume.
struct WorldPreflight {
    source: PathBuf,
    home: PathBuf,
}

/// Everything a spawn path needs from the already-locked world record.
struct ServiceSpawn<'a> {
    world: &'a WorldHandle,
    init_pid: u32,
    ctl_dir: &'a Path,
    state_dir: &'a Path,
    rootfs: &'a WorldRootfs,
    mounts: &'a [WorldMount],
    env: &'a BTreeMap<String, String>,
    spec: &'a ServiceSpec,
    log: &'a ServiceLogs,
    process: &'a Arc<Mutex<ServiceProcess>>,
}

/// The `crun` invocation coordinates of one running backing-service container.
#[derive(Debug, Clone)]
struct ContainerProcess {
    id: String,
    /// World PID 1 used to `nsenter` back into the world for `crun kill` / `exec`.
    init_pid: u32,
    /// Guest path of the pinned static `crun`.
    crun_guest: PathBuf,
    /// Guest path of the `crun` state root (`--root`).
    crun_root_guest: PathBuf,
    /// Host path of the synthesized bundle (removed after the container exits).
    bundle_host: PathBuf,
}

impl BtrfsEngine {
    /// Construct the engine over the concrete [`KernelBtrfsDriver`] injected by the composition
    /// root — the one dependency-injection seam for the CoW substrate (`ADR-0010` §5).
    ///
    /// This is the **only** constructor. Construction probes `statfs(<state_dir>)` through the
    /// driver and fails closed **before** creating a snapshot, world subvolume, or control
    /// directory; there is no clock to wire, because the engine records the plain [`Instant`] it
    /// observes when it spawns each service.
    pub fn new(
        state_dir: impl Into<PathBuf>,
        host_uid: u32,
        host_gid: u32,
        world_init_bin: impl Into<PathBuf>,
        driver: KernelBtrfsDriver,
    ) -> Result<Self, EngineError> {
        let state_dir = state_dir.into();
        let world_init_bin = world_init_bin.into();
        for path in [&state_dir, &world_init_bin] {
            if !path.is_absolute() {
                return Err(EngineError::Io {
                    path: path.display().to_string(),
                    message: "must be absolute (the Btrfs engine never resolves a path against \
                              the ambient current_dir())"
                        .to_owned(),
                });
            }
        }
        // Fail closed before anything exists (`ADR-0010` §4): a non-Btrfs `<state_dir>` never
        // reaches a snapshot or a world directory, and no fallback filesystem is ever used.
        driver.ensure_supported(&state_dir)?;
        Ok(Self {
            state_dir,
            host_uid,
            host_gid,
            world_init_bin,
            driver,
            shared: Arc::new(Shared::default()),
        })
    }

    /// `<state_dir>/snapshots`: parent of every sealed read-only snapshot subvolume.
    fn snapshots_dir(&self) -> PathBuf {
        self.state_dir.join("snapshots")
    }

    /// `<state_dir>/worlds`: parent of every live writable world subvolume.
    fn worlds_dir(&self) -> PathBuf {
        self.state_dir.join("worlds")
    }

    /// `<state_dir>/ctl/<world_id>`: ephemeral per-world control directory, never sealed.
    fn ctl_dir(&self, world_id: &str) -> PathBuf {
        self.state_dir.join("ctl").join(world_id)
    }

    /// `<state_dir>/worlds/<world_id>`: the one writable world subvolume.
    fn world_dir(&self, world_id: &str) -> PathBuf {
        self.worlds_dir().join(world_id)
    }

    /// Stable subvolume name for one world: `<target slug>-<sequence>`.
    fn world_id(&self, target: &WorldTarget, seq: u64) -> String {
        format!("{}-{seq}", target_slug(target))
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<u64, WorldRecord>>, EngineError> {
        self.shared
            .worlds
            .lock()
            .map_err(|_| EngineError::Execution("btrfs world registry lock is poisoned".to_owned()))
    }

    fn owned<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        if !world.belongs_to(self.name()) {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        worlds
            .get_mut(&world.seq())
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })
    }

    /// The record of a world that still exists and whose last reload did not fail, failing closed
    /// with [`EngineError::WorldGone`] once its teardown has started and with
    /// [`EngineError::WorldNotLive`] after a reload whose boot failed.
    ///
    /// Status is **not** a liveness proof: a reload that dies while stopping services or reaping
    /// the PID 1 returns before it stamps `Failed`, so a `Live` record can describe a world whose
    /// init is gone. Callers that must know ask [`WorldEngine::world_is_live`].
    fn live_record<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        let record = self.owned(worlds, world)?;
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        if record.status == WorldStatus::Failed {
            return Err(EngineError::WorldNotLive {
                world: format!("{world:?}"),
            });
        }
        Ok(record)
    }

    /// The seal precondition: only a `Live` world can be sealed, a gone world reports
    /// [`EngineError::WorldGone`], and an already-sealed one [`EngineError::WorldNotFreezable`].
    fn sealable(record: &WorldRecord, world: &WorldHandle) -> Result<(), EngineError> {
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        match record.status {
            WorldStatus::Live => Ok(()),
            WorldStatus::Freezing | WorldStatus::Frozen => Err(EngineError::WorldNotFreezable {
                world: format!("{world:?}"),
            }),
            WorldStatus::Failed => Err(EngineError::WorldNotLive {
                world: format!("{world:?}"),
            }),
            WorldStatus::Destroyed => Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            }),
        }
    }

    /// The reload precondition, the same lifecycle gate [`Self::sealable`] enforces for freeze:
    /// only a `Live` world can be re-rooted, and a world whose previous reload failed to boot may
    /// retry. A sealing/sealed world would be re-materialized over a read-only subvolume, so it
    /// fails closed instead.
    fn reloadable(record: &WorldRecord, world: &WorldHandle) -> Result<(), EngineError> {
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        match record.status {
            WorldStatus::Live | WorldStatus::Failed => Ok(()),
            WorldStatus::Freezing | WorldStatus::Frozen => Err(EngineError::WorldNotReloadable {
                world: format!("{world:?}"),
            }),
            WorldStatus::Destroyed => Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            }),
        }
    }

    /// The record a reload re-roots: [`Self::reloadable`], failing closed before any mutation.
    fn reload_record<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        let record = self.owned(worlds, world)?;
        Self::reloadable(record, world)?;
        Ok(record)
    }

    /// The one `nsenter` argv entering every namespace of `init_pid` at a working directory that
    /// exists **inside** the world — shared by interactive entry, `exec`, and every supervised
    /// `L0` process, so no caller can drift back to a host pathname.
    ///
    /// `--ipc` joins the world's private System V IPC namespace, so `shmget` keys never collide
    /// across parallel worlds (`ADR-0010` §6). The working directory is the load-bearing part:
    /// `nsenter --wd=<dir>` opens the directory in the caller's namespace and only `fchdir(2)`s
    /// after `setns(2)`, so a host pathname becomes a detached dentry once the world's
    /// `pivot_root` is in force and the entered shell reports a `getcwd` failure before running
    /// anything. `--wdns=<dir>` is opened **after** the namespace switch instead, so the process
    /// starts on a real world dentry: [`world_guest_for_host`] maps the caller's host directory through the world's bind mounts, and `/` (the app stage root) is the one
    /// directory every rootfs carries when the caller's directory is not mounted in-world.
    fn entry_prefix(&self, init_pid: u32, mounts: &[WorldMount], host_cwd: &Path) -> Vec<String> {
        let world_cwd =
            world_guest_for_host(mounts, host_cwd).unwrap_or_else(|| PathBuf::from("/"));
        vec![
            format!("--target={init_pid}"),
            "--user".to_owned(),
            "--preserve-credentials".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            format!("--wdns={}", world_cwd.display()),
            "--".to_owned(),
        ]
    }

    /// Full `unshare` argv for the world PID 1 (`ADR-0010` §6).
    ///
    /// Identity UID/GID mapping (`$UID -> $UID`) plus `--keep-caps` preserves exact `$UID:$GID`
    /// ownership on every file written to a shared mount and grants `CAP_SYS_ADMIN` /
    /// `CAP_NET_ADMIN` / `CAP_NET_BIND_SERVICE` inside the user namespace. `--ipc` gives the world
    /// a private System V shared memory and POSIX message-queue namespace: without it, parallel
    /// `postgres` containers in separate network namespaces still collide on `shmget` key `90000`
    /// and port-derived keys.
    fn world_init_argv(&self, config_path: &Path) -> Vec<String> {
        vec![
            "--user".to_owned(),
            format!("--map-user={}", self.host_uid),
            format!("--map-group={}", self.host_gid),
            "--keep-caps".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            "--fork".to_owned(),
            "--kill-child=SIGKILL".to_owned(),
            "--mount-proc".to_owned(),
            self.world_init_bin.display().to_string(),
            config_path.display().to_string(),
        ]
    }

    /// Roll a failed seal back to `Live` so a transient filesystem error does not brick the world.
    fn reopen_after_failed_freeze(&self, world: &WorldHandle) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
            && record.status == WorldStatus::Freezing
            && !record.destroying
        {
            record.status = WorldStatus::Live;
        }
    }

    /// Materialize the world's required layout inside its single subvolume.
    ///
    /// `<subvol>/rootfs` references the content-addressed, immutable `L0` tree in
    /// `<state_dir>/cache/rootfs/<digest>/app` read-only: the cache is the one copy of the image,
    /// so a clone never duplicates gigabytes of rootfs content and never mutates the cache. A
    /// clone of an already-sealed world inherits that reference, and it is repointed at the spec's
    /// tree so a rebuilt image never boots a stale rootfs. `state/{pgdata,redis,s3}` are plain
    /// directories — never subvolumes — so one `BTRFS_IOC_SNAP_CREATE_V2` captures the whole world
    /// (`ADR-0010` §2).
    fn materialize_world(&self, world_dir: &Path, spec: &WorldSpec) -> Result<(), EngineError> {
        self.validate_rootfs_reference(&spec.rootfs)?;
        let rootfs = world_dir.join(WORLD_ROOTFS_DIR);
        match fs::symlink_metadata(&rootfs) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                fs::remove_file(&rootfs).map_err(|err| io_error(&rootfs, &err))?;
            }
            Ok(_) => {
                return Err(EngineError::Io {
                    path: rootfs.display().to_string(),
                    message: "the cloned world carries a rootfs that is not the engine's \
                              read-only reference; a world subvolume holds a reference to the \
                              content-addressed cache, never a second copy of the image"
                        .to_owned(),
                });
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(&spec.rootfs.app_rootfs, &rootfs).map_err(|err| {
            EngineError::Io {
                path: rootfs.display().to_string(),
                message: format!(
                    "cannot materialize the world rootfs reference to '{}': {err}",
                    spec.rootfs.app_rootfs.display()
                ),
            }
        })?;
        for category in WORLD_STATE_CATEGORIES {
            let dir = world_dir.join(WORLD_STATE_DIR).join(category);
            fs::create_dir_all(&dir).map_err(|err| io_error(&dir, &err))?;
        }
        Ok(())
    }

    /// The world's `rootfs` reference must name the content-addressed cache tree this engine owns:
    /// `<state_dir>/cache/rootfs/<digest>/app`. Anything else would make a world boot a tree the
    /// engine never materialized (or a mutable tree inside the subvolume), so it fails closed
    /// before a subvolume is created.
    fn validate_rootfs_reference(&self, rootfs: &WorldRootfs) -> Result<(), EngineError> {
        let expected = self
            .state_dir
            .join("cache")
            .join("rootfs")
            .join(rootfs.digest.hex())
            .join("app");
        if rootfs.app_rootfs == expected {
            return Ok(());
        }
        Err(EngineError::InvalidWorldSpec {
            reason: format!(
                "rootfs '{}' is not the content-addressed cache tree '{}' for digest {}; a world \
                 references the immutable cache read-only and never copies or substitutes a \
                 rootfs tree",
                rootfs.app_rootfs.display(),
                expected.display(),
                rootfs.digest
            ),
        })
    }

    /// Validate `spec` and resolve its clone source and world-scoped `HOME` **before any
    /// mutation**: `spec.validate()`, `HOME` presence, an existing sealed snapshot, and the
    /// content-addressed cache rootfs reference.
    ///
    /// Called first by both [`WorldEngine::clone_world`] and [`WorldEngine::reload_world`] so both
    /// verbs enforce identical preconditions before stopping services or touching subvolumes.
    fn world_preflight(&self, spec: &WorldSpec) -> Result<WorldPreflight, EngineError> {
        spec.validate()?;
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME (ExecEnv::world_scoped)"
                    .to_owned(),
            })?
            .to_path_buf();
        let source = self.snapshots_dir().join(spec.snapshot.validate()?);
        if !source.is_dir() {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        self.validate_rootfs_reference(&spec.rootfs)?;
        Ok(WorldPreflight { source, home })
    }

    /// Materialize the world's control directory, boot the PID 1, and wait until its listeners are
    /// ready. Returns the dial socket path, the verified PID 1 identity, and its launcher.
    fn boot_world(
        &self,
        world_id: &str,
        world_dir: &Path,
        ctl_dir: &Path,
        spec: &WorldSpec,
    ) -> Result<(PathBuf, ProcessIdentity, Child), EngineError> {
        let label = world_id;
        let netns_sock = ctl_dir.join(WORLD_INIT_SOCKET_FILE);
        if !unix_socket_path_fits(&netns_sock) {
            return Err(EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "world dial socket '{}' exceeds the {} byte AF_UNIX path limit; use a \
                     shorter --state-dir",
                    netns_sock.display(),
                    crate::world_init::UNIX_SOCKET_PATH_MAX
                ),
            });
        }
        // A re-used world id must never inherit a previous boot's `init.ready` or `netns.sock`:
        // the control directory is ephemeral, so it is recreated from scratch.
        if ctl_dir.exists() {
            fs::remove_dir_all(ctl_dir).map_err(|err| io_error(ctl_dir, &err))?;
        }
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        let world_rootfs = world_dir.join(WORLD_ROOTFS_DIR);
        let crun_guest = spec.rootfs.crun_bin.clone();
        let crun_host = world_rootfs.join(crun_guest.strip_prefix("/").unwrap_or(&crun_guest));
        if !crun_host.is_file() {
            return Err(EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "the world rootfs '{}' does not contain the pinned static crun at \
                     '{}' ({})",
                    world_rootfs.display(),
                    crun_guest.display(),
                    crun_host.display()
                ),
            });
        }
        let mountpoints = ctl_dir.join("mountpoints");
        let merged = ctl_dir.join("merged");
        let oci_dir = ctl_dir.join("oci");
        let oci_run = ctl_dir.join("oci-run");
        for dir in [&mountpoints, &merged, &oci_dir, &oci_run] {
            fs::create_dir_all(dir).map_err(|err| io_error(dir, &err))?;
        }
        let (passwd, group) =
            synthesize_identity_files(ctl_dir, &spec.rootfs, &home, self.host_uid, self.host_gid)?;

        let state_dir = world_dir.join(WORLD_STATE_DIR);
        let mut mounts: Vec<WorldInitMount> = spec.mounts.iter().map(world_init_mount).collect();
        mounts.extend(spec.rootfs.host_caches.iter().map(world_init_mount));
        mounts.push(WorldInitMount {
            host_path: home.clone(),
            guest_path: home.clone(),
            read_only: false,
        });
        mounts.push(WorldInitMount {
            host_path: passwd.clone(),
            guest_path: PathBuf::from("/etc/passwd"),
            read_only: true,
        });
        mounts.push(WorldInitMount {
            host_path: group.clone(),
            guest_path: PathBuf::from("/etc/group"),
            read_only: true,
        });
        mounts.push(WorldInitMount {
            host_path: passwd,
            guest_path: PathBuf::from(PASSWD_GUEST),
            read_only: true,
        });
        mounts.push(WorldInitMount {
            host_path: group,
            guest_path: PathBuf::from(GROUP_GUEST),
            read_only: true,
        });
        mounts.push(WorldInitMount {
            host_path: state_dir,
            guest_path: PathBuf::from(WORLD_STATE_GUEST),
            read_only: false,
        });
        mounts.push(WorldInitMount {
            host_path: oci_dir,
            guest_path: PathBuf::from(OCI_BUNDLES_GUEST),
            read_only: false,
        });
        mounts.push(WorldInitMount {
            host_path: oci_run,
            guest_path: PathBuf::from(OCI_RUN_GUEST),
            read_only: false,
        });
        for image in spec.rootfs.service_images.values() {
            mounts.push(WorldInitMount {
                host_path: image.rootfs.clone(),
                guest_path: image_guest_rootfs(&image.digest)?,
                read_only: true,
            });
        }
        let local_ingress = match &spec.local_ingress {
            Some(bridge) => {
                if !bridge.host_ingress_sock.exists() {
                    return Err(EngineError::WorldInitFailed {
                        world: label.to_owned(),
                        reason: format!(
                            "local ingress socket '{}' does not exist; bind it before booting a \
                             world",
                            bridge.host_ingress_sock.display()
                        ),
                    });
                }
                mounts.push(WorldInitMount {
                    host_path: bridge.host_ingress_sock.clone(),
                    guest_path: PathBuf::from(INGRESS_GUEST),
                    read_only: true,
                });
                Some(WorldInitIngress {
                    port: bridge.port,
                    host_ingress_sock: bridge.host_ingress_sock.clone(),
                    guest_ingress_sock: PathBuf::from(INGRESS_GUEST),
                })
            }
            None => None,
        };
        let config = WorldInitConfig {
            netns_sock: netns_sock.clone(),
            rootfs: WorldInitRootfs {
                app_rootfs: world_rootfs,
                mountpoints,
                merged,
            },
            mounts,
            local_ingress,
        };
        let config_path = ctl_dir.join(WORLD_INIT_CONFIG_FILE);
        fs::write(
            &config_path,
            serde_json::to_vec(&config).map_err(|err| EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!("cannot serialize world init config: {err}"),
            })?,
        )
        .map_err(|err| EngineError::WorldInitFailed {
            world: label.to_owned(),
            reason: format!("cannot write {}: {err}", config_path.display()),
        })?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!("cannot chmod {} to 0600: {err}", config_path.display()),
            }
        })?;

        // `stdin` is a process-scoped lifeline pipe (`O_CLOEXEC` in the engine, inherited as fd 0
        // by `slipfork-world-init`): unlike `PR_SET_PDEATHSIG` (which Linux fires on creating
        // *thread* exit, breaking RPC thread-per-connection servers), the pipe write end closes
        // only on `destroy_world`, engine `Drop`, or daemon exit/crash.
        let mut launcher = Command::new("unshare")
            .args(self.world_init_argv(&config_path))
            .env_clear()
            .env("PATH", HELPER_PATH)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "cannot spawn `unshare` for `{WORLD_INIT_PROGRAM}` ({}): {err}",
                    self.world_init_bin.display()
                ),
            })?;

        let deadline = Instant::now() + WORLD_INIT_TIMEOUT;
        let mut init: Option<ProcessIdentity> = None;
        // `true` once the `unshare` launcher itself exited before the world was ready: only that
        // failure is classified as a host user-namespace refusal (`ADR-0010` §7).
        let mut unshare_failed = false;
        let outcome: Result<ProcessIdentity, String> = loop {
            if init.is_none() {
                match host_child_pids(launcher.id()) {
                    Ok(children) => {
                        init = children.first().copied().and_then(ProcessIdentity::capture);
                    }
                    Err(err) => {
                        break Err(format!(
                            "cannot read /proc/{}/task/{}/children to discover the world PID 1 \
                             (CONFIG_PROC_CHILDREN or /proc access required): {err}",
                            launcher.id(),
                            launcher.id()
                        ));
                    }
                }
            }
            if let Some(identity) = &init
                && identity.matches_live()
                && fs::read_to_string(config.ready_path()).is_ok_and(|ready| ready == "ready\n")
                && netns_sock.exists()
                // The marker alone is not proof: a listener that cannot accept a connection is
                // not ready, so probe the dial socket before declaring the world up.
                && UnixStream::connect(&netns_sock).is_ok()
            {
                break Ok(identity.clone());
            }
            match launcher.try_wait() {
                Ok(Some(status)) => {
                    unshare_failed = true;
                    break Err(format!(
                        "the world init exited with {status} before becoming ready"
                    ));
                }
                Ok(None) => {}
                Err(err) => break Err(format!("cannot observe the world init process: {err}")),
            }
            if Instant::now() >= deadline {
                let init_pid = init.as_ref().map(|identity| identity.pid);
                break Err(format!(
                    "timed out after {}s waiting for {} (init pid {init_pid:?})",
                    WORLD_INIT_TIMEOUT.as_secs(),
                    config.ready_path().display()
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };

        match outcome {
            Ok(init) => Ok((netns_sock, init, launcher)),
            Err(reason) => {
                let stderr =
                    Self::abandon_unbooted_world(init.as_ref(), &mut launcher).unwrap_or_default();
                let reason = format!("{reason}; helper stderr: {stderr}");
                // `ADR-0010` §7: an unprivileged `CLONE_NEWUSER` refusal (AppArmor
                // `kernel.apparmor_restrict_unprivileged_userns=1`) is a host-profile problem, not
                // a world bug, so it carries the one remediation formatter. Only `unshare`'s own
                // failure is classified; a world-init failure keeps its own diagnostic.
                if unshare_failed && userns_denied(&stderr) {
                    let bin_dir = self.world_init_bin.parent().unwrap_or(Path::new("/"));
                    return Err(EngineError::UsernsDenied {
                        detail: reason,
                        hint: userns_remediation_hint(bin_dir),
                    });
                }
                Err(EngineError::WorldInitFailed {
                    world: label.to_owned(),
                    reason,
                })
            }
        }
    }

    /// Start one `L0` app component (`ServiceRuntime::WorldProcess`) as `$UID:$GID`.
    fn spawn_world_process(
        &self,
        spawn: &ServiceSpawn<'_>,
        argv: &[String],
        cwd: &Path,
    ) -> Result<(), EngineError> {
        // The world PID namespace every descendant of this component lives in. Group membership is
        // preserved across `nsenter --pid` (members report pgid 0 inside the world); the namespace
        // link is what proves a surviving member of a recycled pgid is not ours.
        let world_pid_ns = std::fs::read_link(format!("/proc/{}/ns/pid", spawn.init_pid)).map_err(
            |err| {
                EngineError::Execution(format!(
                    "cannot read the world PID namespace of pid {} to supervise service '{}': {err}",
                    spawn.init_pid, spawn.spec.id
                ))
            },
        )?;
        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(spawn.init_pid, spawn.mounts, cwd))
            .args(argv)
            .env_clear()
            .envs(spawn.env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // One process group per component (`pgid == the launcher's host pid`): `sh -eu -c`
            // forks the real service, so stop must signal the group, never only the first child.
            .process_group(0);
        let mut child = command.spawn().map_err(|err| {
            EngineError::Execution(format!(
                "cannot start service '{}' ({}): {err}",
                spawn.spec.id,
                argv.join(" ")
            ))
        })?;
        let supervisor_pid = child.id();
        let Some(process_group) =
            ProcessIdentity::capture_group_leader(supervisor_pid, world_pid_ns)
        else {
            let label = format!("service '{}'", spawn.spec.id);
            let _ = kill_and_reap_child(&mut child, SERVICE_STOP_GRACE, &label);
            return Err(EngineError::Execution(format!(
                "cannot pin the process group of service '{}' (pid {supervisor_pid}); the spawn \
                 was stopped instead of leaving an unsignalable service",
                spawn.spec.id
            )));
        };
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, crate::StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, crate::StreamSource::Stderr, spawn.log.clone());
        }
        if let Ok(mut process) = spawn.process.lock() {
            process.supervisor = Some(child);
            process.process_group = Some(process_group);
        }
        // `nsenter --pid` forks: the service itself is the launcher's first child. It is recorded
        // for reporting; stop signals the group above.
        if let Some(inner) = wait_for_inner_pid(supervisor_pid, SERVICE_STOP_GRACE)
            .and_then(ProcessIdentity::capture)
            && let Ok(mut process) = spawn.process.lock()
        {
            process.inner = Some(inner);
        }
        Ok(())
    }

    /// Start one daemonless `crun` backing-service container (`ServiceRuntime::OciContainer`).
    fn spawn_oci_container(
        &self,
        spawn: &ServiceSpawn<'_>,
        image: &BackingServiceKey,
        argv: &[String],
        state_mounts: &[StateSubdirMount],
        readonly_mounts: &[WorldMount],
    ) -> Result<(), EngineError> {
        let oci = spawn.rootfs.service_images.get(image).ok_or_else(|| {
            EngineError::InvalidWorldSpec {
                reason: format!(
                    "service '{}' requests backing-service image {image}, but WorldSpec.rootfs \
                     carries no materialized OCI rootfs for it; materialize it with \
                     BaseImageBuilder before cloning the world",
                    spawn.spec.id
                ),
            }
        })?;
        let image_guest = image_guest_rootfs(&oci.digest)?;
        if !oci.rootfs.is_dir() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "backing-service image {image} rootfs '{}' does not exist",
                    oci.rootfs.display()
                ),
            });
        }
        let container_id = container_id(spawn.world.seq(), &spawn.spec.id);
        // Bundles and `crun` state live in the control directory, never in the world subvolume, so
        // a seal never captures OCI runtime state (`ADR-0010` §2).
        let bundle_host = spawn.ctl_dir.join("oci").join(&container_id);
        fs::create_dir_all(&bundle_host).map_err(|err| io_error(&bundle_host, &err))?;
        let bundle_guest = PathBuf::from(OCI_BUNDLES_GUEST).join(&container_id);

        let mut mounts = Vec::with_capacity(state_mounts.len() + readonly_mounts.len());
        for state in state_mounts {
            oci::validate_state_subdir(&state.host_subdir)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            oci::validate_container_path(&state.container_path)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            let host = spawn.state_dir.join(&state.host_subdir);
            fs::create_dir_all(&host).map_err(|err| io_error(&host, &err))?;
            ensure_container_destination(oci, image, &state.container_path)?;
            mounts.push(ContainerMount {
                source: PathBuf::from(WORLD_STATE_GUEST).join(&state.host_subdir),
                destination: state.container_path.clone(),
                read_only: false,
            });
        }
        for readonly in readonly_mounts {
            if readonly.mode != MountMode::ReadOnly {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "service '{}' readonly mount '{}' must carry MountMode::ReadOnly",
                        spawn.spec.id,
                        readonly.host_path.display()
                    ),
                });
            }
            oci::validate_container_path(&readonly.guest_path)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            ensure_container_destination(oci, image, &readonly.guest_path)?;
            let source =
                world_guest_for_host(spawn.mounts, &readonly.host_path).ok_or_else(|| {
                    EngineError::InvalidWorldSpec {
                        reason: format!(
                            "readonly mount '{}' is not visible inside the world; add it to \
                             WorldSpec.mounts as MountMode::ReadOnly",
                            readonly.host_path.display()
                        ),
                    }
                })?;
            for state in state_mounts {
                if oci::paths_overlap(&readonly.guest_path, &state.container_path) {
                    return Err(EngineError::InvalidWorldSpec {
                        reason: format!(
                            "readonly mount {} collides with state mount {} in service '{}'",
                            readonly.guest_path.display(),
                            state.container_path.display(),
                            spawn.spec.id
                        ),
                    });
                }
            }
            mounts.push(ContainerMount {
                source,
                destination: readonly.guest_path.clone(),
                read_only: true,
            });
        }

        oci::write_bundle(&ContainerBundle {
            bundle_dir: &bundle_host,
            image_rootfs: &oci.rootfs,
            image_rootfs_guest: &image_guest,
            argv,
            env: spawn.env,
            uid: self.host_uid,
            gid: self.host_gid,
            mounts: &mounts,
            passwd_guest: Path::new(PASSWD_GUEST),
            group_guest: Path::new(GROUP_GUEST),
        })
        .map_err(|err| io_error(&bundle_host, &err))?;

        let mut command = self.crun_command(
            spawn.init_pid,
            &spawn.rootfs.crun_bin,
            Path::new(OCI_RUN_GUEST),
            spawn.env,
        );
        command
            .args([
                "run".to_owned(),
                "-b".to_owned(),
                bundle_guest.display().to_string(),
                container_id.clone(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| {
            EngineError::Execution(format!(
                "cannot start backing-service container '{}' ({image}): {err}",
                spawn.spec.id
            ))
        })?;
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, crate::StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, crate::StreamSource::Stderr, spawn.log.clone());
        }
        if let Ok(mut process) = spawn.process.lock() {
            process.supervisor = Some(child);
            process.container = Some(ContainerProcess {
                id: container_id,
                init_pid: spawn.init_pid,
                crun_guest: spawn.rootfs.crun_bin.clone(),
                crun_root_guest: PathBuf::from(OCI_RUN_GUEST),
                bundle_host,
            });
        }
        Ok(())
    }

    /// The one `nsenter` → `crun` prefix: enter the world's namespaces, then run the pinned static
    /// `crun` with its disabled cgroup manager and state root. Every `crun` invocation (`run`,
    /// `exec`, `kill`, `delete`) is built from it.
    fn crun_prefix(&self, init_pid: u32, crun_guest: &Path, crun_root_guest: &Path) -> Vec<String> {
        // `crun` is a world binary and each container's own cwd comes from its OCI bundle, so the
        // shared entry prefix only has to land on the app stage root.
        let mut prefix = self.entry_prefix(init_pid, &[], Path::new("/"));
        prefix.push(crun_guest.display().to_string());
        prefix.extend([
            "--cgroup-manager=disabled".to_owned(),
            "--root".to_owned(),
            crun_root_guest.display().to_string(),
        ]);
        prefix
    }

    /// Build a `crun` invocation that runs inside the world's mount/net/ipc/pid namespaces.
    fn crun_command(
        &self,
        init_pid: u32,
        crun_guest: &Path,
        crun_root_guest: &Path,
        env: &BTreeMap<String, String>,
    ) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(self.crun_prefix(init_pid, crun_guest, crun_root_guest))
            .env_clear()
            .envs(env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST);
        command
    }

    fn container_for(
        &self,
        service: &ServiceHandle,
    ) -> Result<(ContainerProcess, ExecEnv), EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, service.world())?;
        let running =
            record
                .services
                .get(service.id())
                .ok_or_else(|| EngineError::UnknownService {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                })?;
        if crate::service_state_from_bits(running.state.load(Ordering::Relaxed))
            != ServiceState::Running
        {
            return Err(EngineError::ServiceNotRunning {
                world: format!("{:?}", service.world()),
                id: service.id().to_owned(),
            });
        }
        let process = running.process.lock().map_err(|_| {
            EngineError::Execution(format!(
                "service '{}' process state is poisoned",
                service.id()
            ))
        })?;
        let container =
            process
                .container
                .clone()
                .ok_or_else(|| EngineError::ServiceNotRunning {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                })?;
        Ok((container, record.base_env.clone()))
    }

    /// Stop `services` in `ADR-0009` §3 order: app components first, then backing-service
    /// containers, so no app request observes a half-shut-down database.
    fn stop_services_with_grace(
        &self,
        services: &[ServiceTeardown],
        grace: Duration,
    ) -> Result<(), EngineError> {
        for kind in [RuntimeKind::WorldProcess, RuntimeKind::OciContainer] {
            for service in services
                .iter()
                .filter(|service| service.runtime_kind == kind)
            {
                self.stop_service_process(
                    &service.id,
                    &service.state,
                    &service.log,
                    &service.process,
                    grace,
                )?;
            }
        }
        Ok(())
    }

    fn stop_service_inner(&self, world: &WorldHandle, id: &str) -> Result<(), EngineError> {
        let (state, log, process) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            let service = record
                .services
                .get(id)
                .ok_or_else(|| EngineError::UnknownService {
                    world: format!("{world:?}"),
                    id: id.to_owned(),
                })?;
            (
                Arc::clone(&service.state),
                service.log.clone(),
                Arc::clone(&service.process),
            )
        };
        self.stop_service_process(id, &state, &log, &process, SERVICE_STOP_GRACE)
    }

    /// Stop one service's process tree with a bounded `SIGTERM` → `SIGKILL` grace.
    ///
    /// The one stop implementation: `grace` is the `SIGTERM` window (`Duration::ZERO` for a spawn
    /// that already failed, which escalates to `SIGKILL` immediately). A world process is
    /// signalled as its whole **process group**, so the `sh -eu -c` wrapper and the service it
    /// forked are both stopped — including a descendant that outlived the supervised child.
    /// Idempotent: a service that already exited on its own, or was stopped before, keeps its
    /// retained logs and reports success once no group member survives. Takes the record's handles
    /// directly so world teardown can stop services after the world has been fenced.
    fn stop_service_process(
        &self,
        id: &str,
        state: &Arc<AtomicU8>,
        log: &ServiceLogs,
        process: &Arc<Mutex<ServiceProcess>>,
        grace: Duration,
    ) -> Result<(), EngineError> {
        let running =
            crate::service_state_from_bits(state.load(Ordering::Relaxed)) == ServiceState::Running;
        let (mut supervisor, process_group, container) = {
            let Ok(mut process) = process.lock() else {
                return Err(EngineError::Execution(format!(
                    "service '{id}' process state is poisoned"
                )));
            };
            (
                process.supervisor.take(),
                process.process_group.clone(),
                process.container.clone(),
            )
        };
        // A row that already left `Running` still owns its group: the supervised child may have
        // exited while a descendant kept running (the `sh`/`python3` shape), so the early return
        // only applies when there is nothing left to reap.
        if !running && supervisor.is_none() && process_group.is_none() && container.is_none() {
            log.close();
            return Ok(());
        }

        if let Some(container) = &container {
            // A backing-service container is signalled through `crun` (not a host process group)
            // and its supervisor is the `nsenter` launcher.
            if grace > Duration::ZERO {
                signal_container(self, container, "TERM");
            }
            let deadline = Instant::now() + grace;
            let mut stopped = grace > Duration::ZERO;
            if stopped && let Some(child) = supervisor.as_mut() {
                stopped = wait_for_exit(child, deadline.saturating_duration_since(Instant::now()));
            }
            if !stopped {
                signal_container(self, container, "KILL");
                if let Some(mut child) = supervisor.take()
                    && let Err(err) = kill_and_reap_child(
                        &mut child,
                        SERVICE_STOP_GRACE,
                        &format!("service '{id}'"),
                    )
                {
                    // The supervisor is restored so a retry can still signal it, and the service
                    // stays `Running` with its log open: marking it `Stopped` here would make a
                    // retried `teardown_world` short-circuit at the "not Running" guard above
                    // while the supervisor still lives.
                    if let Ok(mut guard) = process.lock() {
                        guard.supervisor = Some(child);
                    }
                    return finish_service_stop(state, log, Err(err));
                }
            }
            delete_container(self, container);
            let _ = fs::remove_dir_all(&container.bundle_host);
            if let Ok(mut guard) = process.lock() {
                guard.process_group = None;
            }
            return finish_service_stop(state, log, Ok(()));
        }

        // A world process is only recorded after a successful group pin:
        // `spawn_world_process` stops and fails closed otherwise, so a row without a group has no
        // honest signal target and must never be reported stopped.
        let Some(group) = process_group else {
            if let Some(child) = supervisor.take()
                && let Ok(mut guard) = process.lock()
            {
                guard.supervisor = Some(child);
            }
            return finish_service_stop(
                state,
                log,
                Err(EngineError::Execution(format!(
                    "service '{id}' has no pinned process group; spawn_world_process records a \
                     group or fails closed rather than leave an unsignalable world process"
                ))),
            );
        };
        let label = format!("service '{id}' process group {}", group.pid);
        let stopped = Self::stop_identity(
            Some(&group),
            &mut supervisor,
            grace,
            SERVICE_STOP_GRACE,
            D_STATE_KILL_GRACE,
            &label,
        );
        if let Err(err) = stopped {
            // The supervisor is restored so a retry can still signal it, and the service stays
            // `Running` with its log open (see the container branch above).
            if let Some(child) = supervisor
                && let Ok(mut guard) = process.lock()
            {
                guard.supervisor = Some(child);
            }
            return finish_service_stop(state, log, Err(err));
        }
        // Nothing in the group survives: drop the handle so a repeated stop (idempotent by
        // contract) never signals a host pgid the kernel may have recycled for a new process.
        if let Ok(mut guard) = process.lock() {
            guard.process_group = None;
        }
        finish_service_stop(state, log, Ok(()))
    }

    /// The one stop path for one supervised process tree: `SIGTERM` the pinned identity (the whole
    /// group when it leads one), wait out `term_grace` for the caller's launcher/supervisor child
    /// and the identity, `SIGKILL` and reap the child, then confirm the identity is gone.
    ///
    /// `d_state_grace` is the caller's `D`-state retry budget: a service group retries for
    /// [`D_STATE_KILL_GRACE`], while the world PID 1 keeps its existing kill grace and reports at
    /// once. [`wait_reaped`] names the surviving pid, its `/proc` state, and the **measured** time
    /// since the first `SIGTERM` (the configured kill and `D`-state budgets sit beside it). The
    /// caller keeps ownership of the child: on error it is still in `child` and can be restored
    /// into the record.
    fn stop_identity(
        identity: Option<&ProcessIdentity>,
        child: &mut Option<Child>,
        term_grace: Duration,
        kill_grace: Duration,
        d_state_grace: Duration,
        label: &str,
    ) -> Result<(), EngineError> {
        let term_started = Instant::now();
        let deadline = term_started + term_grace;
        if let Some(identity) = identity {
            let _ = identity.signal(ProcessSignal::Terminate);
        }
        let mut stopped = true;
        if let Some(child) = child.as_mut() {
            stopped = wait_for_exit(child, term_grace);
        }
        if let Some(identity) = identity {
            stopped =
                identity.wait_gone(deadline.saturating_duration_since(Instant::now())) && stopped;
        }
        if !stopped {
            if let Some(identity) = identity
                && let Err(err) = identity.signal(ProcessSignal::Kill)
                && (err.raw_os_error() != Some(libc::ESRCH) || identity.is_live())
            {
                return Err(EngineError::Execution(format!(
                    "cannot SIGKILL {label} (pid {}): {err}",
                    identity.pid
                )));
            }
            if let Some(child) = child.as_mut() {
                kill_and_reap_child(child, kill_grace, label)?;
            }
        }
        match identity {
            Some(identity) => wait_reaped(identity, term_started, kill_grace, d_state_grace, label),
            None => Ok(()),
        }
    }

    /// Abandon a world that will never become a record: drop the `stdin` lifeline, `SIGKILL` the
    /// discovered init identity, kill the `unshare` launcher, and wait for it bounded by
    /// [`WORLD_STOP_KILL_GRACE`].
    ///
    /// Two sites reach this before a `WorldRecord` exists — a `boot_world` that failed its
    /// readiness contract and a `clone_world` whose record could not be inserted — so neither can
    /// reap through [`Self::reap_world_init`] (it owns a record). Each caller keeps its own control
    /// directory and diagnostic handling; the returned `stderr` tail is `None` when the launcher
    /// wrote nothing.
    fn abandon_unbooted_world(
        init: Option<&ProcessIdentity>,
        launcher: &mut Child,
    ) -> Option<String> {
        drop(launcher.stdin.take());
        if let Some(identity) = init {
            let _ = identity.signal(ProcessSignal::Kill);
        }
        let _ = launcher.kill();
        let stderr = drain_stderr(launcher);
        let _ = wait_for_exit(launcher, WORLD_STOP_KILL_GRACE);
        (!stderr.is_empty()).then_some(stderr)
    }

    fn restore_launcher(&self, world: &WorldHandle, launcher: Child) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
        {
            record.launcher = Some(launcher);
        }
    }

    /// Shut down the persistent world PID 1 over its `stdin` lifeline pipe and reap its `unshare`
    /// launcher, escalating to `SIGKILL` after `term_grace`.
    ///
    /// PID 1 of a child PID namespace carries `SIGNAL_UNKILLABLE` (`pid_namespaces(7)`): the
    /// kernel silently drops unhandled `SIGTERM` from an ancestor namespace, whereas `SIGKILL` is
    /// forcibly delivered. Closing `launcher.stdin` makes `slipfork-world-init` observe `EOF` and
    /// exit `0` voluntarily so `unshare` reaps it immediately; `SIGTERM` is also sent (after
    /// `/proc/<pid>/ns/pid` identity verification) for un-namespaced unit-test doubles. The stop
    /// itself is [`Self::stop_identity`] — the same one path a service group takes — with this
    /// world-init grace policy: `term_grace`, then `WORLD_STOP_KILL_GRACE`, no `D`-state retry
    /// budget. Any `kill` or bounded-wait failure restores `launcher` into the record and returns
    /// a typed [`EngineError::Execution`] naming the survivor before storage is touched.
    fn reap_world_init(
        &self,
        world: &WorldHandle,
        term_grace: Duration,
    ) -> Result<(), EngineError> {
        let (mut launcher, init_pid, init_identity) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            (
                record.launcher.take(),
                record.init_pid,
                record.init_identity.clone(),
            )
        };
        if launcher.is_none() && init_pid <= 1 && init_identity.is_none() {
            return Ok(());
        }
        if launcher.is_none() && init_identity.is_none() {
            return Err(EngineError::Execution(format!(
                "cannot reap world init pid {init_pid}: missing /proc/{init_pid}/ns/pid identity"
            )));
        }

        if let Some(child) = launcher.as_mut() {
            drop(child.stdin.take());
        }
        let label = format!("world init pid {init_pid}");
        let stopped = Self::stop_identity(
            init_identity.as_ref(),
            &mut launcher,
            term_grace,
            WORLD_STOP_KILL_GRACE,
            Duration::ZERO,
            &label,
        );
        if let Err(err) = stopped {
            if let Some(child) = launcher {
                self.restore_launcher(world, child);
            }
            return Err(err);
        }

        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        record.init_pid = 0;
        record.init_identity = None;
        Ok(())
    }

    /// Tear down one world's services, persistent PID 1 + `unshare` launcher, control directory,
    /// and world subvolume in `ADR-0010` §3 order, removing the world record once complete.
    ///
    /// Shared by [`WorldEngine::destroy_world`] and [`Drop`]: any failed process or storage step
    /// aborts before removing the record so a retried `destroy_world` resumes where it stopped.
    fn teardown_world(
        &self,
        world: &WorldHandle,
        service_grace: Duration,
        init_term_grace: Duration,
    ) -> Result<(), EngineError> {
        let (world_id, ctl_dir, has_subvolume, services) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            // Fence the record: every later operation on this handle fails closed with `WorldGone`
            // while teardown releases the world's processes and storage. A record already fenced is
            // re-entered, so a failed teardown is always retryable instead of leaking the world.
            record.destroying = true;
            let services: Vec<ServiceTeardown> = record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect();
            (
                record.world_id.clone(),
                record.ctl_dir.clone(),
                matches!(record.subvolume, WorldSubvolume::Cloned(_)),
                services,
            )
        };

        // 1. Stop services (app components first, then backing-service containers).
        self.stop_services_with_grace(&services, service_grace)?;
        // 2. Reap the persistent PID 1 (and with it the whole PID namespace) and its launcher.
        self.reap_world_init(world, init_term_grace)?;
        // 3. Drop the ephemeral control directory (sockets, config, OCI runtime state).
        if ctl_dir.exists() {
            fs::remove_dir_all(&ctl_dir).map_err(|err| io_error(&ctl_dir, &err))?;
        }
        // 4. Destroy the world subvolume when present; the driver clears `BTRFS_SUBVOL_RDONLY` first.
        if has_subvolume {
            self.driver
                .destroy_subvolume(&self.worlds_dir(), &world_id)?;
        }

        // The world subvolume is gone, so the record must go too: a long-lived daemon creates and
        // destroys worlds forever, and the old handle now resolves to `EngineError::WorldGone`
        // exactly like every other operation on a destroyed world.
        self.lock()?.remove(&world.seq());
        Ok(())
    }

    /// `true` when `path` is a subvolume this engine manages under `parent`.
    fn is_managed_subvolume(&self, parent: &Path, path: &Path) -> Result<bool, EngineError> {
        Ok(self
            .driver
            .list_children(parent)?
            .iter()
            .any(|child| child == path))
    }

    /// Seal `source` into `<state_dir>/snapshots/<name>`, refusing to replace an existing snapshot
    /// (`ADR-0010` §3: a sealed snapshot is immutable).
    fn seal_snapshot(&self, source: &Path, name: &str) -> Result<(), EngineError> {
        let snapshots = self.snapshots_dir();
        fs::create_dir_all(&snapshots).map_err(|err| io_error(&snapshots, &err))?;
        if snapshots.join(name).exists() {
            return Err(EngineError::SnapshotExists {
                id: name.to_owned(),
            });
        }
        self.driver
            .snapshot_ro(source, &snapshots, name)
            .map(|_| ())
    }

    /// The first free `<slug>-<seq>` world id, skipping names a previous daemon left behind so a
    /// restarted `slipforkd` never collides with an existing subvolume.
    fn allocate_world_id(&self, target: &WorldTarget) -> Result<(u64, String), EngineError> {
        let worlds = self.worlds_dir();
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        let existing = self.driver.list_children(&worlds)?;
        let mut seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        loop {
            let world_id = self.world_id(target, seq);
            let taken = existing.iter().any(|child| {
                child
                    .file_name()
                    .is_some_and(|name| name == OsStr::new(&world_id))
            });
            if !taken {
                return Ok((seq, world_id));
            }
            seq += 1;
        }
    }
}

impl WorldEngine for BtrfsEngine {
    fn name(&self) -> &'static str {
        "btrfs"
    }

    fn snapshot_exists(&self, id: &SnapshotId) -> Result<bool, EngineError> {
        let name = id.validate()?;
        Ok(self.snapshots_dir().join(name).is_dir())
    }

    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
        let name = id.validate()?;
        let snapshots = self.snapshots_dir();
        let worlds = self.worlds_dir();
        fs::create_dir_all(&snapshots).map_err(|err| io_error(&snapshots, &err))?;
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        if !source_dir.is_dir() {
            return Err(EngineError::Io {
                path: source_dir.display().to_string(),
                message: "snapshot source must be an existing directory".to_owned(),
            });
        }
        // A sealed snapshot is immutable (`ADR-0010` §3): never replace one, and never stage an
        // import that could not be sealed anyway.
        if snapshots.join(name).exists() {
            return Err(EngineError::SnapshotExists {
                id: name.to_owned(),
            });
        }
        // A snapshot source is either a subvolume this engine already manages (under `worlds/` or
        // `snapshots/`) or a plain external directory (the daemon's base-seed directory). A
        // managed subvolume is sealed with one ioctl and never byte-copied.
        for parent in [&worlds, &snapshots] {
            if source_dir.parent() != Some(parent.as_path()) {
                continue;
            }
            if !self.is_managed_subvolume(parent, source_dir)? {
                return Err(EngineError::Io {
                    path: source_dir.display().to_string(),
                    message: format!(
                        "snapshot source is not a managed subvolume under '{}' or '{}'; a managed \
                         subvolume is sealed with one ioctl, and only a plain external directory \
                         (the base-seed directory) is imported",
                        worlds.display(),
                        snapshots.display()
                    ),
                });
            }
            return self.seal_snapshot(source_dir, name);
        }
        // A plain external directory is imported into one staging subvolume, sealed read-only, and
        // dropped: a snapshot is always a subvolume, so every clone and freeze path speaks the
        // same `BTRFS_IOC_SNAP_CREATE_V2` contract.
        let staging = format!("stage-{name}");
        let staging_path = worlds.join(&staging);
        if staging_path.exists() {
            self.driver.destroy_subvolume(&worlds, &staging)?;
        }
        let imported = (|| {
            self.driver.create_subvolume(&worlds, &staging)?;
            copy_tree(source_dir, &staging_path)?;
            self.seal_snapshot(&staging_path, name)
        })();
        let destroyed = self.driver.destroy_subvolume(&worlds, &staging);
        match (imported, destroyed) {
            (Ok(()), Ok(())) => Ok(()),
            // A failed cleanup is reported, never hidden: the driver names the residue.
            (Ok(()), Err(err)) => Err(err),
            (Err(err), _) => Err(err),
        }
    }

    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError> {
        let name = id.validate()?;

        // Services are stopped while the world is still live so the sealed subvolume is
        // crash-consistent (`ADR-0010` §3).
        {
            let mut worlds = self.lock()?;
            Self::sealable(self.owned(&mut worlds, world)?, world)?;
        }
        self.stop_world_services(world)?;

        let world_dir = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            Self::sealable(record, world)?;
            record.status = WorldStatus::Freezing;
            record.world_dir.clone()
        };

        if let Err(err) = self.seal_snapshot(&world_dir, name) {
            self.reopen_after_failed_freeze(world);
            return Err(err);
        }

        let sealed = {
            let mut worlds = self.lock()?;
            match self.owned(&mut worlds, world) {
                Ok(record) if record.is_gone() => Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                }),
                Ok(record) if record.status == WorldStatus::Freezing => {
                    record.status = WorldStatus::Frozen;
                    Ok(())
                }
                Ok(_) => Err(EngineError::WorldNotFreezable {
                    world: format!("{world:?}"),
                }),
                Err(err) => Err(err),
            }
        };
        if let Err(err) = sealed {
            // The world was destroyed while the seal was running: drop the half-claimed snapshot
            // and fail closed instead of resurrecting a destroyed world.
            let _ = self.driver.destroy_subvolume(&self.snapshots_dir(), name);
            return Err(err);
        }
        Ok(())
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        let preflight = self.world_preflight(spec)?;
        fs::create_dir_all(&preflight.home).map_err(|err| io_error(&preflight.home, &err))?;

        let worlds = self.worlds_dir();
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        // One ioctl clones the whole world subvolume: rootfs reference plus state directories. A
        // name a concurrent creator took between the listing and the ioctl is skipped, not fatal.
        let mut attempts = 0;
        let (seq, world_id) = loop {
            let (seq, world_id) = self.allocate_world_id(&spec.target)?;
            match self
                .driver
                .snapshot_writable(&preflight.source, &worlds, &world_id)
            {
                Ok(_) => break (seq, world_id),
                Err(EngineError::Ioctl { errno, .. }) if errno == libc::EEXIST && attempts < 8 => {
                    attempts += 1;
                }
                Err(err) => return Err(err),
            }
        };
        let world = self.mint_world_handle(seq);
        let world_dir = self.world_dir(&world_id);
        let ctl_dir = self.ctl_dir(&world_id);

        let booted = self
            .materialize_world(&world_dir, spec)
            .and_then(|()| self.boot_world(&world_id, &world_dir, &ctl_dir, spec));
        let (netns_sock, init, mut launcher) = match booted {
            Ok(booted) => booted,
            Err(err) => {
                let _ = self.driver.destroy_subvolume(&worlds, &world_id);
                let _ = fs::remove_dir_all(&ctl_dir);
                return Err(err);
            }
        };

        let mut locked = match self.lock() {
            Ok(locked) => locked,
            Err(err) => {
                let _ = Self::abandon_unbooted_world(Some(&init), &mut launcher);
                let _ = fs::remove_dir_all(&ctl_dir);
                let _ = self.driver.destroy_subvolume(&worlds, &world_id);
                return Err(err);
            }
        };
        let init_pid = init.pid;
        locked.insert(
            seq,
            WorldRecord {
                world_id,
                world_dir: world_dir.clone(),
                ctl_dir,
                state_dir: world_dir.join(WORLD_STATE_DIR),
                netns_sock,
                init_pid,
                init_identity: Some(init),
                launcher: Some(launcher),
                base_env: spec.base_env.clone(),
                rootfs: spec.rootfs.clone(),
                mounts: spec.mounts.clone(),
                status: WorldStatus::Live,
                destroying: false,
                services: BTreeMap::new(),
                lineage: boot_lineage(spec),
                subvolume: WorldSubvolume::Cloned(spec.snapshot.clone()),
            },
        );
        Ok(world)
    }

    fn reload_world(&self, world: &WorldHandle, spec: &WorldSpec) -> Result<(), EngineError> {
        // Shared pre-flight runs **before any mutation** (identical to `clone_world`): an invalid
        // spec, missing `HOME`, foreign rootfs, or pruned snapshot fails before touching services,
        // PID 1, or the world subvolume.
        let preflight = self.world_preflight(spec)?;
        let (world_id, world_dir, ctl_dir, services) = {
            let mut worlds = self.lock()?;
            let record = self.reload_record(&mut worlds, world)?;
            let services: Vec<ServiceTeardown> = record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect();
            (
                record.world_id.clone(),
                record.world_dir.clone(),
                record.ctl_dir.clone(),
                services,
            )
        };
        // 1. Stop every world-owned service, 2. reap the world PID 1: nothing from the old run may
        // hold the rootfs reference or a state directory while it is reconciled.
        self.stop_services_with_grace(&services, SERVICE_STOP_GRACE)?;
        self.reap_world_init(world, WORLD_STOP_TERM_GRACE)?;
        let current_subvolume = {
            let mut worlds = self.lock()?;
            let record = self.reload_record(&mut worlds, world)?;
            record.services.clear();
            record.status = WorldStatus::Failed;
            record.subvolume.clone()
        };

        let booted = (|| {
            // Reconcile the subvolume with `spec.snapshot`, recording each on-disk storage
            // transition as soon as it succeeds so a later failure never leaves stale base/rootfs
            // claims over replaced (or absent) storage.
            match current_subvolume {
                WorldSubvolume::Cloned(existing) if existing == spec.snapshot => {}
                WorldSubvolume::Cloned(_) => {
                    self.driver
                        .destroy_subvolume(&self.worlds_dir(), &world_id)?;
                    {
                        let mut worlds = self.lock()?;
                        let record = self.reload_record(&mut worlds, world)?;
                        record.subvolume = WorldSubvolume::Missing;
                        record.lineage = WorldLineage {
                            rootfs: None,
                            services: Vec::new(),
                            seed: None,
                            base: None,
                        };
                    }
                    self.driver.snapshot_writable(
                        &preflight.source,
                        &self.worlds_dir(),
                        &world_id,
                    )?;
                    let mut worlds = self.lock()?;
                    let record = self.reload_record(&mut worlds, world)?;
                    record.subvolume = WorldSubvolume::Cloned(spec.snapshot.clone());
                }
                WorldSubvolume::Missing => {
                    self.driver.snapshot_writable(
                        &preflight.source,
                        &self.worlds_dir(),
                        &world_id,
                    )?;
                    let mut worlds = self.lock()?;
                    let record = self.reload_record(&mut worlds, world)?;
                    record.subvolume = WorldSubvolume::Cloned(spec.snapshot.clone());
                }
            }

            self.materialize_world(&world_dir, spec)?;
            {
                let mut worlds = self.lock()?;
                let record = self.reload_record(&mut worlds, world)?;
                record.rootfs = spec.rootfs.clone();
                record.lineage = boot_lineage(spec);
            }
            fs::create_dir_all(&preflight.home).map_err(|err| io_error(&preflight.home, &err))?;

            self.boot_world(&world_id, &world_dir, &ctl_dir, spec)
        })();

        match booted {
            Ok((netns_sock, init, launcher)) => {
                let mut worlds = self.lock()?;
                let record = self.reload_record(&mut worlds, world)?;
                record.netns_sock = netns_sock;
                record.init_pid = init.pid;
                record.init_identity = Some(init);
                record.launcher = Some(launcher);
                record.mounts = spec.mounts.clone();
                record.base_env = spec.base_env.clone();
                record.status = WorldStatus::Live;
                Ok(())
            }
            Err(err) => {
                // No PID 1 survived this reload: keep only the storage/rootfs/lineage facts that
                // actually hold on disk, ensure the record stays non-live (`WorldStatus::Failed`
                // so a retry recomputes `same_base` from truth and `destroy_world` still owns it),
                // and drop the half-built control directory.
                if let Ok(mut worlds) = self.lock()
                    && let Ok(record) = self.owned(&mut worlds, world)
                {
                    record.init_pid = 0;
                    record.init_identity = None;
                    record.launcher = None;
                    record.status = WorldStatus::Failed;
                }
                let _ = fs::remove_dir_all(&ctl_dir);
                Err(err)
            }
        }
    }

    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError> {
        if spec.id.is_empty() {
            return Err(EngineError::InvalidWorldSpec {
                reason: "service id must not be empty".to_owned(),
            });
        }
        match &spec.runtime {
            ServiceRuntime::WorldProcess { argv, .. }
            | ServiceRuntime::OciContainer { argv, .. } => {
                if argv.is_empty() {
                    return Err(EngineError::InvalidWorldSpec {
                        reason: format!("service '{}' argv must not be empty", spec.id),
                    });
                }
            }
        }
        let runtime_kind = match spec.runtime {
            ServiceRuntime::WorldProcess { .. } => RuntimeKind::WorldProcess,
            ServiceRuntime::OciContainer { .. } => RuntimeKind::OciContainer,
        };

        let (init_pid, ctl_dir, state_dir, rootfs, mounts, env, log, state, process) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            if let Some(existing) = record.services.get(&spec.id)
                && crate::service_state_from_bits(existing.state.load(Ordering::Relaxed))
                    == ServiceState::Running
            {
                return Err(EngineError::ServiceAlreadyRunning {
                    world: format!("{world:?}"),
                    id: spec.id.clone(),
                });
            }
            let init_pid = record.init_pid;
            let ctl_dir = record.ctl_dir.clone();
            let state_dir = record.state_dir.clone();
            let rootfs = record.rootfs.clone();
            let mounts = record.mounts.clone();
            let env = child_env::materialize(&record.base_env, &spec.env, |var| {
                std::env::var(var.name()).ok()
            });
            let log = ServiceLogs::new(spec.id.as_str());
            let state = Arc::new(AtomicU8::new(ServiceState::Running.to_bits()));
            let process = Arc::new(Mutex::new(ServiceProcess::default()));
            record.services.insert(
                spec.id.clone(),
                ServiceRecord {
                    runtime_kind,
                    spec: spec.clone(),
                    kind: spec.kind,
                    port: spec.readiness_port,
                    started_at: Instant::now(),
                    log: log.clone(),
                    state: Arc::clone(&state),
                    process: Arc::clone(&process),
                },
            );
            (
                init_pid, ctl_dir, state_dir, rootfs, mounts, env, log, state, process,
            )
        };

        let spawn = ServiceSpawn {
            world,
            init_pid,
            ctl_dir: &ctl_dir,
            state_dir: &state_dir,
            rootfs: &rootfs,
            mounts: &mounts,
            env: &env,
            spec,
            log: &log,
            process: &process,
        };
        let spawned = match &spec.runtime {
            ServiceRuntime::WorldProcess { argv, cwd } => {
                self.spawn_world_process(&spawn, argv, cwd)
            }
            ServiceRuntime::OciContainer {
                image,
                argv,
                state_mounts,
                readonly_mounts,
            } => self.spawn_oci_container(&spawn, image, argv, state_mounts, readonly_mounts),
        };
        if let Err(err) = spawned {
            state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
            log.close();
            return Err(err);
        }
        spawn_service_watcher(Arc::clone(&process), Arc::clone(&state), log.clone());

        if let Some(port) = spec.readiness_port {
            let timeout = if runtime_kind == RuntimeKind::OciContainer {
                OCI_SERVICE_READY_TIMEOUT
            } else {
                SERVICE_READY_TIMEOUT
            };
            let deadline = Instant::now() + timeout;
            loop {
                match self.dial_world_tcp(world, port) {
                    Ok(_probe) => break,
                    Err(EngineError::DialFailed { .. }) => {}
                    Err(err) => {
                        let _ = self.stop_service_process(
                            &spec.id,
                            &state,
                            &log,
                            &process,
                            Duration::ZERO,
                        );
                        return Err(err);
                    }
                }
                if crate::service_state_from_bits(state.load(Ordering::Relaxed))
                    != ServiceState::Running
                {
                    // The watcher already reaped the process and closed the log; the record stays
                    // `Exited` so a corrected retry with the same id is not blocked.
                    return Err(EngineError::Execution(format!(
                        "service '{}' exited before port {port} accepted connections (captured \
                         logs retained)",
                        spec.id
                    )));
                }
                if Instant::now() >= deadline {
                    // Never leave a half-started service running behind a failed spawn: kill and
                    // reap it, mark it `Stopped`, and keep its captured log for diagnosis so the
                    // caller can retry the same id.
                    let _ =
                        self.stop_service_process(&spec.id, &state, &log, &process, Duration::ZERO);
                    return Err(EngineError::Execution(format!(
                        "service '{}' did not accept connections on port {port} within {}s; \
                         the process was stopped (captured logs retained)",
                        spec.id,
                        timeout.as_secs()
                    )));
                }
                thread::sleep(POLL_INTERVAL);
            }
        }

        Ok(ServiceHandle::mint(world.clone(), spec.id.clone(), state))
    }

    fn update_service_env(
        &self,
        service: &ServiceHandle,
        env: &ExecEnv,
    ) -> Result<(), EngineError> {
        let mut spec =
            {
                let mut worlds = self.lock()?;
                let record = self.live_record(&mut worlds, service.world())?;
                let service_record = record.services.get(service.id()).ok_or_else(|| {
                    EngineError::UnknownService {
                        world: format!("{:?}", service.world()),
                        id: service.id().to_owned(),
                    }
                })?;
                service_record.spec.clone()
            };
        // The one stop path first, so a failed respawn never leaves two processes under one id.
        self.stop_service_inner(service.world(), service.id())?;
        spec.env = env.clone();
        self.spawn_service(service.world(), &spec)?;
        Ok(())
    }

    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError> {
        self.stop_service_inner(service.world(), service.id())
    }

    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let services: Vec<ServiceTeardown> = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            // Every recorded service, not only the `Running` rows: an `Exited` component whose
            // descendant outlived its supervisor (the `sh`-gone/`python3`-alive shape) still owns
            // a process group, and a freeze that sealed over it would capture a live writer.
            record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect()
        };
        // The one stop path: components first, then containers (`ADR-0009` §3).
        self.stop_services_with_grace(&services, SERVICE_STOP_GRACE)
    }

    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        record
            .services
            .get(id)
            .map(|service| service.log.clone())
            .ok_or_else(|| EngineError::UnknownService {
                world: format!("{world:?}"),
                id: id.to_owned(),
            })
    }

    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        let now = Instant::now();
        let mut rows = Vec::with_capacity(record.services.len());
        for (id, service) in &record.services {
            let state = crate::service_state_from_bits(service.state.load(Ordering::Relaxed));
            let running = state == ServiceState::Running;
            // A stopped or self-exited service keeps its row but reports no live facts: `None`
            // renders `-`, never a stale pid or an uptime that keeps growing.
            let pid = if running {
                let process = service.process.lock().map_err(|_| {
                    // A poisoned supervision record cannot say whether the row is live: fail
                    // closed instead of reporting `Running` with a missing pid.
                    EngineError::Execution(format!(
                        "service '{id}' supervision record in world {world:?} is poisoned"
                    ))
                })?;
                match service.runtime_kind {
                    RuntimeKind::WorldProcess => process.inner.as_ref().map(|inner| inner.pid),
                    RuntimeKind::OciContainer => process.supervisor.as_ref().map(Child::id),
                }
            } else {
                None
            };
            rows.push(WorldServiceInfo {
                id: id.clone(),
                kind: service.kind,
                state,
                pid,
                port: running.then_some(service.port).flatten(),
                uptime_secs: running
                    .then(|| now.saturating_duration_since(service.started_at).as_secs()),
            });
        }
        Ok(rows)
    }

    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        Ok(record.lineage.clone())
    }

    fn world_is_live(&self, world: &WorldHandle) -> Result<bool, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        // Both halves are load-bearing: a reload stamps `Failed` only *after* it reaped the PID 1,
        // so a `Live` status alone can still describe a dead world, and a live pin alone cannot
        // vouch for a record being sealed mid-freeze. Leader-only: a service left behind by dead
        // init is not a live world.
        Ok(record.status == WorldStatus::Live
            && record
                .init_identity
                .as_ref()
                .is_some_and(ProcessIdentity::is_live_leader))
    }

    fn exec_in_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "service exec argv must not be empty".to_owned(),
            ));
        }
        let (container, base_env) = self.container_for(service)?;
        let env =
            child_env::materialize(&base_env, overlay_env, |var| std::env::var(var.name()).ok());
        let mut command = self.crun_command(
            container.init_pid,
            &container.crun_guest,
            &container.crun_root_guest,
            &env,
        );
        command.arg("exec");
        for (name, value) in &env {
            command.args(["--env", &format!("{name}={value}")]);
        }
        command
            .arg(&container.id)
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed(command, on_output)
    }

    fn exec_in_service_with_input(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        input: Box<dyn std::io::Read + Send>,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "service exec argv must not be empty".to_owned(),
            ));
        }
        let (container, base_env) = self.container_for(service)?;
        let env =
            child_env::materialize(&base_env, overlay_env, |var| std::env::var(var.name()).ok());
        let mut command = self.crun_command(
            container.init_pid,
            &container.crun_guest,
            &container.crun_root_guest,
            &env,
        );
        command.arg("exec");
        for (name, value) in &env {
            command.args(["--env", &format!("{name}={value}")]);
        }
        command
            .arg(&container.id)
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed_with_input(command, input, on_output)
    }

    fn service_entry_command(
        &self,
        service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError> {
        let (container, _env) = self.container_for(service)?;
        let mut args_prefix = self.crun_prefix(
            container.init_pid,
            &container.crun_guest,
            &container.crun_root_guest,
        );
        args_prefix.extend(["exec".to_owned(), container.id, "--".to_owned()]);
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix,
            cwd: PathBuf::from("/"),
        })
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "exec argv must not be empty".to_owned(),
            ));
        }
        let (init_pid, mounts, env) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            (
                record.init_pid,
                record.mounts.clone(),
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    std::env::var(var.name()).ok()
                }),
            )
        };

        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, &mounts, cwd))
            .args(argv)
            .env_clear()
            .envs(&env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed(command, on_output)
    }

    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        let (init_pid, mounts) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            (record.init_pid, record.mounts.clone())
        };
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix: self.entry_prefix(init_pid, &mounts, cwd),
            // The thin CLI `chdir`s here before `exec`ing `nsenter`, so this must be a host path:
            // `/` is the one directory that exists on the host *and* in every world rootfs. The
            // in-world start directory is `--wdns=<mapped dir>` inside `args_prefix`.
            cwd: PathBuf::from("/"),
        })
    }

    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<TcpStream, EngineError> {
        let netns_sock = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            record.netns_sock.clone()
        };
        let mut socket = UnixStream::connect(&netns_sock).map_err(|err| EngineError::Io {
            path: netns_sock.display().to_string(),
            message: err.to_string(),
        })?;
        socket
            .set_read_timeout(Some(DIAL_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(DIAL_TIMEOUT)))
            .map_err(|err| EngineError::Io {
                path: netns_sock.display().to_string(),
                message: err.to_string(),
            })?;
        socket
            .write_all(&port.to_be_bytes())
            .map_err(|err| EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: err.raw_os_error().unwrap_or(libc::EIO),
            })?;
        let (status, descriptor) =
            linux_cow::recv_fd(&socket).map_err(|err| EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: err.raw_os_error().unwrap_or(libc::EIO),
            })?;
        if status != linux_cow::STATUS_OK {
            return Err(EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: i32::from(status),
            });
        }
        let descriptor = descriptor.ok_or_else(|| {
            EngineError::Execution(format!(
                "world init answered dial for port {port} without an SCM_RIGHTS descriptor"
            ))
        })?;
        Ok(TcpStream::from(descriptor))
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        self.teardown_world(&world, SERVICE_STOP_GRACE, WORLD_STOP_TERM_GRACE)
    }
}

fn container_control(engine: &BtrfsEngine, container: &ContainerProcess, args: &[&str]) -> Command {
    let mut command = Command::new("nsenter");
    command
        .args(engine.crun_prefix(
            container.init_pid,
            &container.crun_guest,
            &container.crun_root_guest,
        ))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn signal_container(engine: &BtrfsEngine, container: &ContainerProcess, signal: &str) {
    let _ = container_control(engine, container, &["kill", &container.id, signal]).status();
}

fn delete_container(engine: &BtrfsEngine, container: &ContainerProcess) {
    let _ = container_control(engine, container, &["delete", "--force", &container.id]).status();
}

/// Mark a service `Exited` (never restarted) once its supervisor process is reaped.
fn spawn_service_watcher(
    process: Arc<Mutex<ServiceProcess>>,
    state: Arc<AtomicU8>,
    log: ServiceLogs,
) {
    thread::spawn(move || {
        loop {
            let exited = {
                let Ok(mut guard) = process.lock() else {
                    return;
                };
                let Some(supervisor) = guard.supervisor.as_mut() else {
                    return;
                };
                match supervisor.try_wait() {
                    Ok(Some(_)) => true,
                    Ok(None) => false,
                    Err(_) => return,
                }
            };
            if exited {
                if crate::service_state_from_bits(state.load(Ordering::Relaxed))
                    == ServiceState::Running
                {
                    state.store(ServiceState::Exited.to_bits(), Ordering::Relaxed);
                    log.close();
                }
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });
}

/// Copy one plain directory tree into a freshly created subvolume.
///
/// Only used when [`WorldEngine::create_snapshot`] imports a source that is not already a managed
/// subvolume (the daemon's empty base-seed directory): a snapshot is always a subvolume, so the
/// one non-CoW source is materialized once instead of teaching every consumer a second kind of
/// snapshot.
fn copy_tree(from: &Path, to: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(to).map_err(|err| io_error(to, &err))?;
    for entry in fs::read_dir(from).map_err(|err| io_error(from, &err))? {
        let entry = entry.map_err(|err| io_error(from, &err))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type().map_err(|err| io_error(&source, &err))?;
        if file_type.is_dir() {
            copy_tree(&source, &target)?;
        } else if file_type.is_file() {
            fs::copy(&source, &target).map_err(|err| io_error(&target, &err))?;
        } else if file_type.is_symlink() {
            let link = fs::read_link(&source).map_err(|err| io_error(&source, &err))?;
            std::os::unix::fs::symlink(link, &target).map_err(|err| io_error(&target, &err))?;
        } else {
            return Err(EngineError::Io {
                path: source.display().to_string(),
                message: "cannot import a snapshot source containing a socket, fifo, or device \
                          node"
                    .to_owned(),
            });
        }
    }
    Ok(())
}

/// The lineage facts a fresh clone records, taken verbatim from the spec it booted from.
///
/// Rootfs/service facts are the materialized trees' own cache outcome and measured `millis`; the
/// base/seed facts are the seeder's measurements. Nothing is estimated here.
fn boot_lineage(spec: &WorldSpec) -> WorldLineage {
    WorldLineage {
        rootfs: Some(RootfsLineage {
            digest: spec.rootfs.digest.hex().to_owned(),
            cache: spec.rootfs.cache,
            millis: spec.rootfs.millis,
        }),
        services: spec
            .rootfs
            .service_images
            .iter()
            .map(|(image, oci)| ServiceImageLineage {
                id: image.to_string(),
                cache: oci.cache,
                millis: oci.millis,
            })
            .collect(),
        seed: spec.boot.seed.clone(),
        base: spec.boot.base.clone(),
    }
}

fn io_error(path: &Path, err: &std::io::Error) -> EngineError {
    EngineError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    }
}

/// Apply the terminal state of one service-stop attempt.
///
/// Only a fully reaped supervisor becomes [`ServiceState::Stopped`] with a closed log; a failed
/// `kill`/reap keeps the service [`ServiceState::Running`] and its log open so a retried
/// teardown re-enters `kill_and_reap_child` instead of short-circuiting on `Stopped` while a
/// supervisor process still lives.
fn finish_service_stop(
    state: &Arc<AtomicU8>,
    log: &ServiceLogs,
    outcome: Result<(), EngineError>,
) -> Result<(), EngineError> {
    if outcome.is_ok() {
        state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
        log.close();
    }
    outcome
}

/// `true` when a failed `unshare` invocation reports a denied user namespace (`EPERM`).
///
/// The kernel and AppArmor both phrase this refusal as `Operation not permitted`; `unshare` prints
/// it to stderr and exits non-zero (`ADR-0010` §7).
fn userns_denied(stderr: &str) -> bool {
    stderr.contains("Operation not permitted") || stderr.contains("EPERM")
}
