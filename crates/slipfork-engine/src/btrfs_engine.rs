//! `BtrfsEngine`: one unified Btrfs subvolume per world (`ADR-0010` §§2–3, `ADR-0002`, `H-011`).
//!
//! Every live world is exactly one writable Btrfs subvolume `<state_dir>/worlds/<world_id>`,
//! cloned from the read-only snapshot `<state_dir>/snapshots/<spec.snapshot>` with one
//! `BTRFS_IOC_SNAP_CREATE_V2` call, and sealed back into `<state_dir>/snapshots/<id>` with
//! `BTRFS_SUBVOL_RDONLY` through the injected [`SubvolumeDriver`]. Inside that single subvolume
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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::WorldTarget;

use crate::btrfs::SubvolumeDriver;
use crate::child_env;
use crate::clock::MonotonicClock;
use crate::linux_cow::{self, ProcessSignal};
use crate::oci::{self, ContainerBundle, ContainerMount};
use crate::service::{
    POLL_INTERVAL, ProcessIdentity, ServiceLogs, container_id, drain_stderr,
    ensure_container_destination, host_child_pids, kill_and_reap_child, run_streamed,
    spawn_log_reader, wait_for_exit, wait_for_inner_pid, world_cwd_wrapper, world_guest_for_host,
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
    driver: Arc<dyn SubvolumeDriver>,
    clock: Arc<dyn MonotonicClock>,
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
    mounts: Vec<WorldMount>,
    status: WorldStatus,
    /// A teardown started and may still be running: every caller sees the world as gone, and
    /// [`WorldEngine::destroy_world`] re-enters where the previous attempt stopped.
    destroying: bool,
    services: BTreeMap<String, ServiceRecord>,
    /// This world's own boot lineage, recorded when [`WorldEngine::clone_world`] materialized it.
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
    /// The caller-declared row classification (`ServiceSpec::kind`).
    kind: ServiceKind,
    /// In-world serving/readiness port the caller asked for, recorded at spawn.
    port: Option<u16>,
    /// Injected-clock instant the service was spawned; `uptime_secs` is measured from it.
    started_at: Instant,
    log: ServiceLogs,
    state: Arc<AtomicU8>,
    process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
struct ServiceProcess {
    supervisor: Option<Child>,
    inner: Option<ProcessIdentity>,
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
    /// Construct the engine over an injected [`SubvolumeDriver`] and [`MonotonicClock`] — the two
    /// dependency-injection seams for the CoW substrate (`ADR-0010` §5) and for every measured
    /// fact.
    ///
    /// This is the **only** constructor: the clock is required, so a composition root that forgets
    /// to wire the one clock cannot compile into a second time source. `slipforkd` injects the one
    /// clock instance it also hands to the rootfs builder, the OCI image store, and its own
    /// base-seed measurement, so `uptime_secs` and every lineage `millis` are stamped from a single
    /// time source; tests inject a deterministic fake. Construction probes `statfs(<state_dir>)`
    /// through the driver and fails closed **before** creating a snapshot, world subvolume, or
    /// control directory.
    pub fn with_driver_and_clock(
        state_dir: impl Into<PathBuf>,
        host_uid: u32,
        host_gid: u32,
        world_init_bin: impl Into<PathBuf>,
        driver: Arc<dyn SubvolumeDriver>,
        clock: Arc<dyn MonotonicClock>,
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
            clock,
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

    /// The record of a world that still exists, failing closed with [`EngineError::WorldGone`] once
    /// its teardown has started.
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
            WorldStatus::Destroyed => Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            }),
        }
    }

    /// `nsenter` arguments entering every namespace of `init_pid`.
    ///
    /// `--ipc` joins the world's private System V IPC namespace, so `shmget` keys never collide
    /// across parallel worlds (`ADR-0010` §6). The working directory is applied by
    /// [`world_cwd_wrapper`] **after** the mount-namespace entry: a path that lives on a bind
    /// mount (the repo, `<state_dir>`) has no reachable host pathname after `pivot_root`, so
    /// entering `--wd=<host path>` leaves `getcwd(2)` failing.
    fn entry_prefix(&self, init_pid: u32, cwd: &Path) -> Vec<String> {
        vec![
            format!("--target={init_pid}"),
            "--user".to_owned(),
            "--preserve-credentials".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            format!("--wd={}", cwd.display()),
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
                drop(launcher.stdin.take());
                if let Some(identity) = &init {
                    let _ = identity.signal(ProcessSignal::Kill);
                }
                let _ = launcher.kill();
                let stderr = drain_stderr(&mut launcher);
                let _ = wait_for_exit(&mut launcher, WORLD_STOP_KILL_GRACE);
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
        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(spawn.init_pid, Path::new("/")))
            .args(world_cwd_wrapper(cwd, argv))
            .env_clear()
            .envs(spawn.env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| {
            EngineError::Execution(format!(
                "cannot start service '{}' ({}): {err}",
                spawn.spec.id,
                argv.join(" ")
            ))
        })?;
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, crate::StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, crate::StreamSource::Stderr, spawn.log.clone());
        }
        let supervisor_pid = child.id();
        if let Ok(mut process) = spawn.process.lock() {
            process.supervisor = Some(child);
        }
        // `nsenter --pid` forks: the service itself is the launcher's first child. `SIGTERM` must
        // reach that in-world pid, never the launcher parent.
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
        let mut prefix = self.entry_prefix(init_pid, Path::new("/"));
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
        if ServiceState::from_bits(running.state.load(Ordering::Relaxed)) != ServiceState::Running {
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

    /// Stop every live service of `world` before its subvolume is sealed.
    fn stop_live_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let services: Vec<ServiceTeardown> = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            record
                .services
                .iter()
                .filter(|(_, service)| {
                    ServiceState::from_bits(service.state.load(Ordering::Relaxed))
                        == ServiceState::Running
                })
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect()
        };
        self.stop_services_with_grace(&services, SERVICE_STOP_GRACE)
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
    /// that already failed, which escalates to `SIGKILL` immediately). Idempotent: a service that
    /// already exited on its own, or was stopped before, keeps its retained logs and reports
    /// success. Takes the record's handles directly so world teardown can stop services after the
    /// world has been fenced.
    fn stop_service_process(
        &self,
        id: &str,
        state: &Arc<AtomicU8>,
        log: &ServiceLogs,
        process: &Arc<Mutex<ServiceProcess>>,
        grace: Duration,
    ) -> Result<(), EngineError> {
        if ServiceState::from_bits(state.load(Ordering::Relaxed)) != ServiceState::Running {
            log.close();
            return Ok(());
        }

        let (supervisor, inner, container) = {
            let Ok(mut process) = process.lock() else {
                return Err(EngineError::Execution(format!(
                    "service '{id}' process state is poisoned"
                )));
            };
            (
                process.supervisor.take(),
                process.inner.clone(),
                process.container.clone(),
            )
        };
        let Some(mut supervisor) = supervisor else {
            state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
            log.close();
            return Ok(());
        };
        // Re-resolve the in-world pid if it was not observed at spawn time: `SIGTERM` must reach
        // the forked service, not only the `nsenter` launcher.
        let inner = inner.or_else(|| {
            host_child_pids(supervisor.id())
                .ok()
                .and_then(|children| children.first().copied())
                .and_then(ProcessIdentity::capture)
        });

        if grace > Duration::ZERO {
            if let Some(container) = &container {
                signal_container(self, container, "TERM");
            } else if let Some(inner) = &inner {
                let _ = inner.signal(ProcessSignal::Terminate);
            }
        }
        if grace.is_zero() || !wait_for_exit(&mut supervisor, grace) {
            if let Some(container) = &container {
                signal_container(self, container, "KILL");
            } else if let Some(inner) = &inner {
                let _ = inner.signal(ProcessSignal::Kill);
            }
            if let Err(err) = kill_and_reap_child(
                &mut supervisor,
                SERVICE_STOP_GRACE,
                &format!("service '{id}'"),
            ) {
                // The supervisor is restored so a retry can still signal it, and the service stays
                // `Running` with its log open: marking it `Stopped` here would make a retried
                // `teardown_world` short-circuit at the "not Running" guard above while the
                // supervisor still lives, and report a clean destroy over a leaked process.
                if let Ok(mut guard) = process.lock() {
                    guard.supervisor = Some(supervisor);
                }
                return finish_service_stop(state, log, Err(err));
            }
        }
        if let Some(container) = &container {
            delete_container(self, container);
            let _ = fs::remove_dir_all(&container.bundle_host);
        }
        finish_service_stop(state, log, Ok(()))
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
    /// `/proc/<pid>/ns/pid` identity verification) for un-namespaced unit-test doubles. If either
    /// process survives `term_grace`, `SIGKILL` is sent to the verified `init` and `launcher`
    /// (`--kill-child=SIGKILL`), and `launcher` is bounded-reaped **before** polling
    /// `/proc/<init_pid>`. Any `kill` or bounded-wait failure restores `launcher` into the record
    /// and returns a typed [`EngineError::Execution`] naming the launcher pid before storage is
    /// touched.
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

        let started = Instant::now();
        if let Some(child) = launcher.as_mut() {
            drop(child.stdin.take());
        }
        if term_grace > Duration::ZERO
            && let Some(identity) = &init_identity
        {
            let _ = identity.signal(ProcessSignal::Terminate);
        }

        let term_deadline = started + term_grace;
        if let Some(mut child) = launcher.take() {
            let exited_on_term =
                term_grace > Duration::ZERO && wait_for_exit(&mut child, term_grace);
            if !exited_on_term {
                if let Some(identity) = &init_identity {
                    let _ = identity.signal(ProcessSignal::Kill);
                }
                let label = format!("world init launcher pid {}", child.id());
                if let Err(err) = kill_and_reap_child(&mut child, WORLD_STOP_KILL_GRACE, &label) {
                    self.restore_launcher(world, child);
                    return Err(err);
                }
            }
        }

        if let Some(identity) = &init_identity {
            let remaining_term = term_deadline.saturating_duration_since(Instant::now());
            if !identity.wait_gone(remaining_term) {
                if let Err(err) = identity.signal(ProcessSignal::Kill)
                    && identity.matches_live()
                {
                    return Err(EngineError::Execution(format!(
                        "cannot SIGKILL world init pid {}: {err}",
                        identity.pid
                    )));
                }
                if !identity.wait_gone(WORLD_STOP_KILL_GRACE) {
                    return Err(EngineError::Execution(format!(
                        "world init pid {} survived SIGTERM and SIGKILL for {:?}",
                        identity.pid,
                        started.elapsed()
                    )));
                }
            }
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
        let (world_id, ctl_dir, services) = {
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
            (record.world_id.clone(), record.ctl_dir.clone(), services)
        };

        // 1. Stop services (app components first, then backing-service containers).
        self.stop_services_with_grace(&services, service_grace)?;
        // 2. Reap the persistent PID 1 (and with it the whole PID namespace) and its launcher.
        self.reap_world_init(world, init_term_grace)?;
        // 3. Drop the ephemeral control directory (sockets, config, OCI runtime state).
        if ctl_dir.exists() {
            fs::remove_dir_all(&ctl_dir).map_err(|err| io_error(&ctl_dir, &err))?;
        }
        // 4. Destroy the world subvolume; the driver clears `BTRFS_SUBVOL_RDONLY` first.
        self.driver
            .destroy_subvolume(&self.worlds_dir(), &world_id)?;

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
        self.stop_live_services(world)?;

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
        spec.validate()?;
        let snapshot_name = spec.snapshot.validate()?;
        let snapshots = self.snapshots_dir();
        let source = snapshots.join(snapshot_name);
        if !source.is_dir() {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        // Fail closed before any subvolume exists: the world may only reference the
        // content-addressed cache tree this engine owns.
        self.validate_rootfs_reference(&spec.rootfs)?;
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        fs::create_dir_all(&home).map_err(|err| io_error(&home, &err))?;

        let worlds = self.worlds_dir();
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        // One ioctl clones the whole world subvolume: rootfs reference plus state directories. A
        // name a concurrent creator took between the listing and the ioctl is skipped, not fatal.
        let mut attempts = 0;
        let (seq, world_id) = loop {
            let (seq, world_id) = self.allocate_world_id(&spec.target)?;
            match self.driver.snapshot_writable(&source, &worlds, &world_id) {
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
                drop(launcher.stdin.take());
                let _ = init.signal(ProcessSignal::Kill);
                let _ = launcher.kill();
                let _ = wait_for_exit(&mut launcher, WORLD_STOP_KILL_GRACE);
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
            },
        );
        Ok(world)
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
                && ServiceState::from_bits(existing.state.load(Ordering::Relaxed))
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
            let log = ServiceLogs::new();
            let state = Arc::new(AtomicU8::new(ServiceState::Running.to_bits()));
            let process = Arc::new(Mutex::new(ServiceProcess::default()));
            record.services.insert(
                spec.id.clone(),
                ServiceRecord {
                    runtime_kind,
                    kind: spec.kind,
                    port: spec.readiness_port,
                    started_at: self.clock.now(),
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
                if ServiceState::from_bits(state.load(Ordering::Relaxed)) != ServiceState::Running {
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

    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError> {
        self.stop_service_inner(service.world(), service.id())
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
        let now = self.clock.now();
        let mut rows = Vec::with_capacity(record.services.len());
        for (id, service) in &record.services {
            let state = ServiceState::from_bits(service.state.load(Ordering::Relaxed));
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
        let (init_pid, env) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            (
                record.init_pid,
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    std::env::var(var.name()).ok()
                }),
            )
        };

        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, Path::new("/")))
            .args(world_cwd_wrapper(cwd, argv))
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
        let init_pid = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            record.init_pid
        };
        let mut args_prefix = self.entry_prefix(init_pid, Path::new("/"));
        args_prefix.extend(world_cwd_wrapper(cwd, &[]));
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix,
            cwd: cwd.to_path_buf(),
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
                if ServiceState::from_bits(state.load(Ordering::Relaxed)) == ServiceState::Running {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::{BTRFS_MOUNT_HINT, ProbeFacts};
    use crate::rootfs::{ElfAbi, ImageContractMetadata, LibcFamily, OsRelease, RootfsDigest};
    use crate::service::ServiceLogs;
    use crate::{
        BackingServiceKey, BaseLineage, BootLineage, CacheOutcome, ExecEnv, SeedLineage,
        ServiceSpec, StateSubdirMount, WorldRootfs,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant, SystemTime};

    const XFS_SUPER_MAGIC: i64 = 0x5846_5342;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DriverOp {
        EnsureSupported {
            path: PathBuf,
        },
        CreateSubvolume {
            path: PathBuf,
        },
        SnapshotRo {
            source: PathBuf,
            dest: PathBuf,
            watched_present: Option<bool>,
        },
        SnapshotWritable {
            source: PathBuf,
            dest: PathBuf,
        },
        DestroySubvolume {
            path: PathBuf,
            watched_present: Option<bool>,
        },
        ListChildren {
            parent: PathBuf,
        },
    }

    /// Minimal directory-backed driver that journals ordering for `--lib` tests.
    ///
    /// The behavioural [`SubvolumeDriver`] double (read-only seal, marker identity, clear-RO before
    /// destroy) is `slipfork_testkit::btrfs::TestTreeDriver` and is exercised by the integration
    /// suites; this fake exists only to observe *when* the engine calls the driver relative to
    /// service teardown and control-directory removal.
    struct RecordingDriver {
        fstype: i64,
        journal: Mutex<Vec<DriverOp>>,
        watch: Mutex<Option<PathBuf>>,
        subvolumes: Mutex<std::collections::BTreeSet<PathBuf>>,
        destroy_failures: Mutex<usize>,
    }

    impl RecordingDriver {
        fn btrfs() -> Self {
            Self {
                fstype: crate::btrfs::BTRFS_SUPER_MAGIC,
                journal: Mutex::new(Vec::new()),
                watch: Mutex::new(None),
                subvolumes: Mutex::new(std::collections::BTreeSet::new()),
                destroy_failures: Mutex::new(0),
            }
        }

        fn non_btrfs() -> Self {
            Self {
                fstype: XFS_SUPER_MAGIC,
                journal: Mutex::new(Vec::new()),
                watch: Mutex::new(None),
                subvolumes: Mutex::new(std::collections::BTreeSet::new()),
                destroy_failures: Mutex::new(0),
            }
        }

        /// Fail the next `count` `destroy_subvolume` calls with `EBUSY` (a retryable teardown).
        fn with_destroy_failures(self, count: usize) -> Self {
            *self.destroy_failures.lock().expect("destroy failures") = count;
            self
        }

        /// Record whether `path` still exists at the next `snapshot_ro` / `destroy_subvolume`.
        fn watch(&self, path: impl Into<PathBuf>) {
            *self.watch.lock().expect("watch") = Some(path.into());
        }

        fn journal(&self) -> Vec<DriverOp> {
            self.journal.lock().expect("journal").clone()
        }

        fn clear_journal(&self) {
            self.journal.lock().expect("journal").clear();
        }

        fn record(&self, op: DriverOp) {
            self.journal.lock().expect("journal").push(op);
        }

        fn watched(&self) -> Option<bool> {
            self.watch
                .lock()
                .expect("watch")
                .as_ref()
                .map(|path| path.exists())
        }

        /// Subvolume identity, like the kernel's: only what this driver created is a subvolume.
        fn mark_subvolume(&self, path: &Path) {
            self.subvolumes
                .lock()
                .expect("subvolumes")
                .insert(path.to_path_buf());
        }
        fn unmark_subvolume(&self, path: &Path) {
            self.subvolumes.lock().expect("subvolumes").remove(path);
        }
    }

    impl SubvolumeDriver for RecordingDriver {
        fn probe_facts(&self, path: &Path) -> Result<ProbeFacts, EngineError> {
            Ok(ProbeFacts {
                probed_path: path.to_path_buf(),
                fstype: self.fstype,
                noatime: true,
            })
        }

        fn ensure_supported(&self, path: &Path) -> Result<(), EngineError> {
            self.record(DriverOp::EnsureSupported {
                path: path.to_path_buf(),
            });
            crate::btrfs::require_btrfs(self.fstype, path)
        }

        fn create_subvolume(&self, parent: &Path, name: &str) -> Result<PathBuf, EngineError> {
            let path = parent.join(name);
            fs::create_dir_all(&path).map_err(|err| io_error(&path, &err))?;
            self.mark_subvolume(&path);
            self.record(DriverOp::CreateSubvolume { path: path.clone() });
            Ok(path)
        }

        fn snapshot_ro(
            &self,
            source_subvol: &Path,
            dest_parent: &Path,
            name: &str,
        ) -> Result<PathBuf, EngineError> {
            let dest = dest_parent.join(name);
            fs::create_dir_all(&dest).map_err(|err| io_error(&dest, &err))?;
            self.mark_subvolume(&dest);
            self.record(DriverOp::SnapshotRo {
                source: source_subvol.to_path_buf(),
                dest: dest.clone(),
                watched_present: self.watched(),
            });
            Ok(dest)
        }

        fn snapshot_writable(
            &self,
            source_subvol: &Path,
            dest_parent: &Path,
            name: &str,
        ) -> Result<PathBuf, EngineError> {
            let dest = dest_parent.join(name);
            fs::create_dir_all(&dest).map_err(|err| io_error(&dest, &err))?;
            self.mark_subvolume(&dest);
            self.record(DriverOp::SnapshotWritable {
                source: source_subvol.to_path_buf(),
                dest: dest.clone(),
            });
            Ok(dest)
        }

        fn destroy_subvolume(&self, parent: &Path, name: &str) -> Result<(), EngineError> {
            let path = parent.join(name);
            self.record(DriverOp::DestroySubvolume {
                path: path.clone(),
                watched_present: self.watched(),
            });
            let mut failures = self.destroy_failures.lock().expect("destroy failures");
            if *failures > 0 {
                *failures -= 1;
                return Err(EngineError::Ioctl {
                    op: "snap_destroy",
                    path,
                    errno: libc::EBUSY,
                });
            }
            drop(failures);
            self.unmark_subvolume(&path);
            match fs::remove_dir_all(&path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(io_error(&path, &err)),
            }
        }

        fn list_children(&self, parent: &Path) -> Result<Vec<PathBuf>, EngineError> {
            self.record(DriverOp::ListChildren {
                parent: parent.to_path_buf(),
            });
            let children: Vec<PathBuf> = self
                .subvolumes
                .lock()
                .expect("subvolumes")
                .iter()
                .filter(|path| path.parent() == Some(parent))
                .cloned()
                .collect();
            Ok(children)
        }
    }

    /// A contract-valid rootfs whose `app_rootfs` is the engine's content-addressed cache tree.
    fn rootfs(engine: &BtrfsEngine) -> WorldRootfs {
        let digest = RootfsDigest::from_hex("0".repeat(64)).expect("digest");
        let app_rootfs = engine
            .state_dir
            .join("cache")
            .join("rootfs")
            .join(digest.hex())
            .join("app");
        WorldRootfs {
            digest,
            app_rootfs,
            crun_bin: PathBuf::from(crate::rootfs::CRUN_GUEST_PATH),
            service_images: BTreeMap::new(),
            metadata: ImageContractMetadata {
                os_release: OsRelease {
                    id: "debian".to_owned(),
                    version_id: "12".to_owned(),
                    version_codename: "bookworm".to_owned(),
                },
                elf_abi: ElfAbi {
                    arch: "x86_64".to_owned(),
                    libc: LibcFamily::Glibc,
                    libc_version: "2.36".to_owned(),
                },
                exposed_ports: std::collections::BTreeSet::new(),
            },
            host_caches: Vec::new(),
            cache: CacheOutcome::Hit,
            millis: None,
        }
    }

    fn engine_at(state_dir: &Path, driver: Arc<RecordingDriver>) -> BtrfsEngine {
        BtrfsEngine::with_driver_and_clock(
            state_dir,
            1000,
            1000,
            state_dir.join("slipfork-world-init"),
            driver,
            Arc::new(crate::SystemClock),
        )
        .expect("engine construction")
    }

    /// A deterministic [`MonotonicClock`]: a fixed base instant advanced only by the test.
    #[derive(Clone)]
    struct FakeClock {
        base: Instant,
        secs: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                secs: Arc::new(AtomicU64::new(0)),
            }
        }

        fn advance(&self, secs: u64) {
            self.secs.fetch_add(secs, Ordering::SeqCst);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_secs(self.secs.load(Ordering::SeqCst))
        }

        fn now_wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(self.secs.load(Ordering::SeqCst))
        }
    }

    fn engine_with_clock(
        state_dir: &Path,
        driver: Arc<RecordingDriver>,
        clock: Arc<dyn MonotonicClock>,
    ) -> BtrfsEngine {
        BtrfsEngine::with_driver_and_clock(
            state_dir,
            1000,
            1000,
            state_dir.join("slipfork-world-init"),
            driver,
            clock,
        )
        .expect("engine construction")
    }

    fn world_record(
        engine: &BtrfsEngine,
        world_id: &str,
        init: Option<(Child, u32)>,
    ) -> WorldRecord {
        let world_dir = engine.world_dir(world_id);
        let ctl_dir = engine.ctl_dir(world_id);
        fs::create_dir_all(world_dir.join(WORLD_STATE_DIR)).expect("world state dir");
        fs::create_dir_all(&ctl_dir).expect("control dir");
        let (launcher, init_pid, init_identity) = match init {
            Some((launcher, init_pid)) => {
                let identity = ProcessIdentity::capture(init_pid);
                (Some(launcher), init_pid, identity)
            }
            None => (None, 0, None),
        };
        WorldRecord {
            world_id: world_id.to_owned(),
            world_dir: world_dir.clone(),
            ctl_dir: ctl_dir.clone(),
            state_dir: world_dir.join(WORLD_STATE_DIR),
            netns_sock: ctl_dir.join(WORLD_INIT_SOCKET_FILE),
            init_pid,
            init_identity,
            launcher,
            base_env: ExecEnv::world_scoped(engine.state_dir.join("homes/test")),
            rootfs: rootfs(engine),
            mounts: Vec::new(),
            status: WorldStatus::Live,
            destroying: false,
            services: BTreeMap::new(),
            lineage: boot_lineage(&WorldSpec {
                target: WorldTarget::Canonical,
                rootfs: rootfs(engine),
                snapshot: SnapshotId("base".to_owned()),
                mounts: Vec::new(),
                base_env: ExecEnv::world_scoped(engine.state_dir.join("homes/test")),
                local_ingress: None,
                boot: BootLineage::default(),
            }),
        }
    }

    fn insert(engine: &BtrfsEngine, record: WorldRecord) -> WorldHandle {
        let seq = engine.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        engine
            .shared
            .worlds
            .lock()
            .expect("world registry")
            .insert(seq, record);
        engine.mint_world_handle(seq)
    }

    /// A supervisor that exits as soon as its in-world child is signalled, plus that child's pid.
    fn running_world_process(
        record: &mut WorldRecord,
        id: &str,
        started_at: Instant,
    ) -> (Arc<AtomicU8>, u32) {
        let supervisor = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .spawn()
            .expect("service supervisor");
        let inner_pid =
            wait_for_inner_pid(supervisor.id(), Duration::from_secs(5)).expect("service inner pid");
        let inner = ProcessIdentity::capture(inner_pid);
        let state = Arc::new(AtomicU8::new(ServiceState::Running.to_bits()));
        let log = ServiceLogs::new();
        let process = Arc::new(Mutex::new(ServiceProcess {
            supervisor: Some(supervisor),
            inner,
            container: None,
        }));
        record.services.insert(
            id.to_owned(),
            ServiceRecord {
                runtime_kind: RuntimeKind::WorldProcess,
                kind: ServiceKind::Component,
                port: None,
                started_at,
                log,
                state: Arc::clone(&state),
                process,
            },
        );
        (state, inner_pid)
    }

    fn stopped_record(kind: ServiceKind, started_at: Instant) -> ServiceRecord {
        ServiceRecord {
            runtime_kind: RuntimeKind::WorldProcess,
            kind,
            port: Some(5432),
            started_at,
            log: ServiceLogs::new(),
            state: Arc::new(AtomicU8::new(ServiceState::Stopped.to_bits())),
            process: Arc::new(Mutex::new(ServiceProcess::default())),
        }
    }

    #[test]
    fn world_processes_keeps_stopped_rows_and_stamps_uptime_from_the_injected_clock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = Arc::new(RecordingDriver::btrfs());
        let clock = Arc::new(FakeClock::new());
        let engine = engine_with_clock(&state_dir, driver, clock.clone());

        let mut record = world_record(&engine, "canonical-1", None);
        let now = clock.now();
        record.services.insert(
            "pg".to_owned(),
            stopped_record(ServiceKind::BackingService, now),
        );
        // Two running services started 5s apart: uptime is measured from each own `started_at`.
        let (_, _) = running_world_process(&mut record, "api", now);
        clock.advance(5);
        let (_, _) = running_world_process(&mut record, "worker", now + Duration::from_secs(5));
        clock.advance(5);
        let handle = insert(&engine, record);

        let rows = engine.world_processes(&handle).expect("process rows");
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            ["api", "pg", "worker"],
            "rows are ordered by service id"
        );

        let pg = rows.iter().find(|row| row.id == "pg").expect("pg row");
        assert_eq!(pg.kind, ServiceKind::BackingService);
        assert_eq!(pg.state, ServiceState::Stopped);
        assert_eq!(pg.pid, None, "a stopped service reports no live pid");
        assert_eq!(pg.uptime_secs, None, "a stopped service has no uptime");
        assert_eq!(pg.port, None, "a stopped service serves no port");

        let api = rows.iter().find(|row| row.id == "api").expect("api row");
        let worker = rows
            .iter()
            .find(|row| row.id == "worker")
            .expect("worker row");
        assert_eq!(api.kind, ServiceKind::Component);
        assert_eq!(api.state, ServiceState::Running);
        assert!(
            api.pid.is_some(),
            "a running component reports its in-world pid"
        );
        assert_eq!(api.uptime_secs, Some(10));
        assert_eq!(worker.uptime_secs, Some(5));

        // A sibling world reports only its own row set: one world never shows another's rows.
        let mut sibling = world_record(&engine, "fork-a-2", None);
        running_world_process(&mut sibling, "other", Instant::now());
        let sibling_world = insert(&engine, sibling);
        let sibling_rows = engine
            .world_processes(&sibling_world)
            .expect("sibling rows");
        assert_eq!(
            sibling_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["other"]
        );

        // A foreign engine tag is gone, exactly like a destroyed handle.
        let foreign = engine.mint_world_handle(999);
        let err = engine
            .world_processes(&foreign)
            .expect_err("an unknown handle must fail closed");
        assert!(matches!(err, EngineError::WorldGone { .. }), "got {err:?}");

        engine.destroy_world(handle.clone()).expect("destroy");
        let err = engine
            .world_processes(&handle)
            .expect_err("a destroyed world must fail closed");
        assert!(matches!(err, EngineError::WorldGone { .. }), "got {err:?}");
    }

    /// The lineage a clone records comes only from the spec it booted from: cache outcomes are the
    /// materialized trees' own verdicts, measured durations are present only when one was measured,
    /// and two worlds never share a fact.
    #[test]
    fn world_lineage_records_each_world_own_boot_facts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = Arc::new(RecordingDriver::btrfs());
        let clock = Arc::new(FakeClock::new());
        let engine = engine_with_clock(&state_dir, driver, clock);

        let rebuilt = engine_spec(
            &engine,
            "rebuilt",
            (CacheOutcome::Miss, Some(2104)),
            (CacheOutcome::Miss, Some(55)),
            BootLineage {
                base: Some(BaseLineage {
                    key: "a".repeat(64),
                    millis: Some(10),
                }),
                seed: Some(SeedLineage {
                    digest: Some("b".repeat(64)),
                    rebuilt: Some(true),
                    millis: Some(2104),
                }),
            },
        );
        let cached = engine_spec(
            &engine,
            "cached",
            (CacheOutcome::Hit, None),
            (CacheOutcome::Hit, None),
            BootLineage {
                base: Some(BaseLineage {
                    key: "c".repeat(64),
                    millis: None,
                }),
                seed: None,
            },
        );

        let mut rebuilt_record = world_record(&engine, "canonical-1", None);
        rebuilt_record.lineage = boot_lineage(&rebuilt);
        let rebuilt_world = insert(&engine, rebuilt_record);

        let mut cached_record = world_record(&engine, "fork-a-2", None);
        cached_record.lineage = boot_lineage(&cached);
        let cached_world = insert(&engine, cached_record);

        let first = engine.world_lineage(&rebuilt_world).expect("lineage");
        assert_eq!(
            first.rootfs,
            Some(RootfsLineage {
                digest: rebuilt.rootfs.digest.hex().to_owned(),
                cache: CacheOutcome::Miss,
                millis: Some(2104),
            })
        );
        // A miss is a measured fact (`Some`); a hit ran no build and records `None`, never `0`.
        let second = engine.world_lineage(&cached_world).expect("lineage");
        let second_rootfs = second.rootfs.as_ref().expect("rootfs facts");
        assert_eq!(second_rootfs.cache, CacheOutcome::Hit);
        assert_eq!(second_rootfs.millis, None);
        assert_eq!(
            second_rootfs.digest,
            cached.rootfs.digest.hex().to_owned(),
            "each world reports its own rootfs digest"
        );

        // The backing-service row is this world's own image verdict, not the sibling's.
        assert_eq!(
            first.services,
            vec![ServiceImageLineage {
                id: "postgres:18".to_owned(),
                cache: CacheOutcome::Miss,
                millis: Some(55),
            }]
        );
        assert_eq!(
            second.services,
            vec![ServiceImageLineage {
                id: "postgres:18".to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }]
        );
        assert_eq!(first.base.as_ref().map(|base| base.millis), Some(Some(10)));
        assert_eq!(second.base.as_ref().map(|base| base.millis), Some(None));
        assert_ne!(first.base, second.base, "no fact is shared between worlds");
        assert_eq!(
            first.seed.as_ref().and_then(|seed| seed.rebuilt),
            Some(true)
        );
        assert_eq!(
            second.seed, None,
            "a world that recorded no seed reports none"
        );

        // A destroyed handle is gone, never a zeroed lineage.
        engine
            .destroy_world(rebuilt_world.clone())
            .expect("destroy");
        let err = engine
            .world_lineage(&rebuilt_world)
            .expect_err("a destroyed world must fail closed");
        assert!(matches!(err, EngineError::WorldGone { .. }), "got {err:?}");
    }

    /// A spec carrying the materialized rootfs facts a boot would have measured.
    fn engine_spec(
        engine: &BtrfsEngine,
        label: &str,
        rootfs_facts: (CacheOutcome, Option<u64>),
        service_facts: (CacheOutcome, Option<u64>),
        boot: BootLineage,
    ) -> WorldSpec {
        let (rootfs_cache, rootfs_millis) = rootfs_facts;
        let (service_cache, service_millis) = service_facts;
        let mut rootfs = rootfs(engine);
        rootfs.cache = rootfs_cache;
        rootfs.millis = rootfs_millis;
        rootfs.service_images.insert(
            BackingServiceKey::Postgres {
                version: "18".to_owned(),
            },
            crate::OciRootfs {
                digest: format!("sha256:{}", "d".repeat(64)),
                rootfs: engine.state_dir.join("cache/oci/probe/rootfs"),
                config: engine.state_dir.join("cache/oci/probe/config.json"),
                cache: service_cache,
                millis: service_millis,
            },
        );
        WorldSpec {
            target: WorldTarget::UserFork(label.to_owned()),
            rootfs,
            snapshot: SnapshotId("base".to_owned()),
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(engine.state_dir.join("homes/test")),
            local_ingress: None,
            boot,
        }
    }

    #[test]
    fn construction_fails_closed_off_btrfs_before_creating_anything() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = Arc::new(RecordingDriver::non_btrfs());
        match BtrfsEngine::with_driver_and_clock(
            &state_dir,
            1000,
            1000,
            tmp.path().join("slipfork-world-init"),
            Arc::clone(&driver) as Arc<dyn SubvolumeDriver>,
            Arc::new(crate::SystemClock),
        ) {
            Err(EngineError::BtrfsRequired {
                path,
                actual_fstype,
                mount_hint,
            }) => {
                assert_eq!(path, state_dir);
                assert_eq!(actual_fstype, XFS_SUPER_MAGIC);
                assert_eq!(mount_hint, BTRFS_MOUNT_HINT);
            }
            other => panic!("expected BtrfsRequired, got {other:?}"),
        }
        assert!(
            !state_dir.exists(),
            "a refused construction creates no state directory"
        );
        assert_eq!(
            driver.journal(),
            vec![DriverOp::EnsureSupported {
                path: state_dir.clone()
            }]
        );
        assert!(
            BTRFS_MOUNT_HINT.contains("user_subvol_rm_allowed,noatime"),
            "the refusal names the exact mount remediation"
        );
    }

    #[test]
    fn relative_paths_are_rejected_without_touching_the_cwd() {
        let driver = Arc::new(RecordingDriver::btrfs());
        match BtrfsEngine::with_driver_and_clock(
            Path::new("state"),
            1000,
            1000,
            Path::new("/usr/libexec/slipfork-world-init"),
            Arc::clone(&driver) as Arc<dyn SubvolumeDriver>,
            Arc::new(crate::SystemClock),
        ) {
            Err(EngineError::Io { path, message }) => {
                assert_eq!(path, "state");
                assert!(message.contains("absolute"), "{message}");
            }
            other => panic!("a relative state dir must be rejected: {other:?}"),
        }
        assert!(
            driver.journal().is_empty(),
            "no driver probe runs for an invalid path"
        );
    }

    #[test]
    fn layout_is_one_subvolume_with_a_disjoint_control_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = engine_at(tmp.path(), Arc::new(RecordingDriver::btrfs()));

        assert_eq!(engine.snapshots_dir(), tmp.path().join("snapshots"));
        assert_eq!(engine.worlds_dir(), tmp.path().join("worlds"));
        assert_eq!(engine.world_id(&WorldTarget::Canonical, 1), "canonical-1");
        assert_eq!(
            engine.world_id(&WorldTarget::UserFork("acme".to_owned()), 2),
            "fork-acme-2"
        );
        assert_eq!(engine.world_id(&WorldTarget::TestSlot(3), 4), "slot-3-4");
        assert_eq!(
            engine.world_dir("slot-3-4"),
            tmp.path().join("worlds/slot-3-4")
        );
        assert_eq!(engine.ctl_dir("slot-3-4"), tmp.path().join("ctl/slot-3-4"));
        assert!(
            !engine
                .ctl_dir("slot-3-4")
                .starts_with(engine.world_dir("slot-3-4")),
            "the control directory is never inside the sealed subvolume"
        );
        assert_eq!(
            engine.world_dir("slot-3-4").join(WORLD_ROOTFS_DIR),
            tmp.path().join("worlds/slot-3-4/rootfs")
        );
        assert_eq!(
            engine.world_dir("slot-3-4").join(WORLD_STATE_DIR),
            tmp.path().join("worlds/slot-3-4/state")
        );
    }

    #[test]
    fn the_world_init_and_every_namespace_entry_gain_a_private_ipc_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = engine_at(tmp.path(), Arc::new(RecordingDriver::btrfs()));

        let argv = engine.world_init_argv(Path::new("/state/ctl/slot-0-1/world-init.json"));
        assert_eq!(
            argv.iter().filter(|arg| *arg == "--ipc").count(),
            1,
            "the world PID 1 must enter a private IPC namespace: {argv:?}"
        );
        for flag in [
            "--user",
            "--mount",
            "--net",
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            "--mount-proc",
            "--keep-caps",
        ] {
            assert!(
                argv.iter().any(|arg| arg == flag),
                "{flag} missing: {argv:?}"
            );
        }
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/state/ctl/slot-0-1/world-init.json")
        );
        assert!(
            argv.iter().any(|arg| arg.ends_with("slipfork-world-init")),
            "the injected helper is the unshare target: {argv:?}"
        );

        let prefix = engine.entry_prefix(4242, Path::new("/"));
        assert_eq!(
            prefix.iter().filter(|arg| *arg == "--ipc").count(),
            1,
            "every nsenter entry joins the world's IPC namespace: {prefix:?}"
        );
        for flag in ["--mount", "--net", "--pid", "--user"] {
            assert!(
                prefix.iter().any(|arg| arg == flag),
                "{flag} missing: {prefix:?}"
            );
        }
    }

    #[test]
    fn freeze_stops_every_service_before_sealing_the_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let mut record = world_record(&engine, "slot-0-1", None);
        let (state, inner_pid) = running_world_process(&mut record, "api", Instant::now());
        let world = insert(&engine, record);
        driver.watch(format!("/proc/{inner_pid}"));

        engine
            .freeze_world(&world, &SnapshotId("slot-0-frozen".to_owned()))
            .expect("a live world must be freezable");

        assert_eq!(
            state.load(Ordering::Relaxed),
            ServiceState::Stopped.to_bits(),
            "freeze stops the service"
        );
        let journal = driver.journal();
        let (dest, watched) = journal
            .iter()
            .find_map(|op| match op {
                DriverOp::SnapshotRo {
                    dest,
                    watched_present,
                    ..
                } => Some((dest.clone(), *watched_present)),
                _ => None,
            })
            .expect("freeze seals the world subvolume");
        assert_eq!(dest, tmp.path().join("snapshots/slot-0-frozen"));
        assert_eq!(
            watched,
            Some(false),
            "the service process must be gone before the seal: {journal:?}"
        );
        let sealed_source = journal
            .iter()
            .find_map(|op| match op {
                DriverOp::SnapshotRo { source, .. } => Some(source.clone()),
                _ => None,
            })
            .expect("seal source");
        assert_eq!(
            sealed_source,
            tmp.path().join("worlds/slot-0-1"),
            "the whole world subvolume is sealed in one call"
        );

        match engine.freeze_world(&world, &SnapshotId("again".to_owned())) {
            Err(EngineError::WorldNotFreezable { .. }) => {}
            other => panic!("a frozen world must fail closed: {other:?}"),
        }
    }

    #[test]
    fn destroy_stops_services_reaps_the_init_drops_the_control_dir_then_the_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        // `sh -c 'sleep 30 & wait'` models the launcher/init pair: `unshare` waits for the world
        // PID 1, so signalling the PID 1 reaps it and the launcher exits.
        let launcher = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .spawn()
            .expect("world init launcher");
        let init_pid =
            wait_for_inner_pid(launcher.id(), Duration::from_secs(5)).expect("world init pid");
        let mut record = world_record(&engine, "slot-0-2", Some((launcher, init_pid)));
        let (state, _inner) = running_world_process(&mut record, "api", Instant::now());
        let ctl_dir = record.ctl_dir.clone();
        fs::write(ctl_dir.join(WORLD_INIT_CONFIG_FILE), b"{}").expect("control marker");
        let world = insert(&engine, record);
        driver.watch(ctl_dir.join(WORLD_INIT_CONFIG_FILE));

        engine.destroy_world(world.clone()).expect("destroy world");

        assert_eq!(
            state.load(Ordering::Relaxed),
            ServiceState::Stopped.to_bits(),
            "destroy stops services first"
        );
        assert!(
            !Path::new(&format!("/proc/{init_pid}")).exists(),
            "the persistent world init is reaped"
        );
        assert!(!ctl_dir.exists(), "the control directory is removed");
        let journal = driver.journal();
        let (destroyed, watched) = journal
            .iter()
            .find_map(|op| match op {
                DriverOp::DestroySubvolume {
                    path,
                    watched_present,
                } => Some((path.clone(), *watched_present)),
                _ => None,
            })
            .expect("the world subvolume is destroyed");
        assert_eq!(destroyed, tmp.path().join("worlds/slot-0-2"));
        assert_eq!(
            watched,
            Some(false),
            "the control directory must be gone before the subvolume is destroyed: {journal:?}"
        );

        match engine.freeze_world(&world, &SnapshotId("gone".to_owned())) {
            Err(EngineError::WorldGone { .. }) => {}
            other => panic!("a destroyed world must fail closed: {other:?}"),
        }
    }

    #[test]
    fn a_world_directory_that_is_not_a_subvolume_is_refused_as_a_snapshot_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let foreign = engine.worlds_dir().join("not-a-subvolume");
        fs::create_dir_all(&foreign).expect("foreign world directory");

        match engine.create_snapshot(&foreign, &SnapshotId("foreign".to_owned())) {
            Err(EngineError::Io { path, message }) => {
                assert_eq!(path, foreign.display().to_string());
                assert!(message.contains("not a managed subvolume"), "{message}");
            }
            other => panic!("a non-subvolume world source must fail closed: {other:?}"),
        }
        assert!(
            !tmp.path().join("snapshots/foreign").exists(),
            "no snapshot is fabricated from a foreign tree"
        );
    }

    #[test]
    fn a_plain_snapshot_source_is_imported_into_one_sealed_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let source = tmp.path().join("base-seed");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("seed.txt"), b"seed\n").expect("seed file");

        engine
            .create_snapshot(&source, &SnapshotId("project-seed-empty".to_owned()))
            .expect("a plain directory is imported and sealed");

        let snapshot = tmp.path().join("snapshots/project-seed-empty");
        assert!(snapshot.is_dir(), "the snapshot is a real subvolume");
        let journal = driver.journal();
        assert!(
            journal
                .iter()
                .any(|op| matches!(op, DriverOp::CreateSubvolume { .. })),
            "a non-subvolume source is staged once: {journal:?}"
        );
        assert!(
            journal.iter().any(|op| matches!(
                op,
                DriverOp::SnapshotRo { source, .. } if source == &tmp.path().join("worlds/stage-project-seed-empty")
            )),
            "the staged subvolume is sealed read-only: {journal:?}"
        );
        assert!(
            journal.iter().any(|op| matches!(
                op,
                DriverOp::DestroySubvolume { path, .. } if path == &tmp.path().join("worlds/stage-project-seed-empty")
            )),
            "the staging subvolume is dropped: {journal:?}"
        );
        assert!(
            !tmp.path().join("worlds/stage-project-seed-empty").exists(),
            "no staging residue"
        );
    }

    #[test]
    fn a_managed_subvolume_source_is_sealed_without_copying() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let world_dir = engine
            .driver
            .create_subvolume(&engine.worlds_dir(), "slot-0-9")
            .expect("world subvolume");
        assert_eq!(world_dir, engine.world_dir("slot-0-9"));
        driver.clear_journal();

        engine
            .create_snapshot(&world_dir, &SnapshotId("slot-0-9-frozen".to_owned()))
            .expect("a managed subvolume is sealed directly");

        let journal = driver.journal();
        assert!(
            !journal
                .iter()
                .any(|op| matches!(op, DriverOp::CreateSubvolume { .. })),
            "a managed subvolume is never copied: {journal:?}"
        );
        assert!(
            journal.iter().any(|op| matches!(
                op,
                DriverOp::SnapshotRo { source, dest, .. }
                    if source == &world_dir && dest == &tmp.path().join("snapshots/slot-0-9-frozen")
            )),
            "the world subvolume is sealed directly: {journal:?}"
        );
    }

    #[test]
    fn clone_world_refuses_an_unknown_snapshot_before_creating_a_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let spec = WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: rootfs(&engine),
            snapshot: SnapshotId("missing".to_owned()),
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(tmp.path().join("home")),
            local_ingress: None,
            boot: BootLineage::default(),
        };

        match engine.clone_world(&spec) {
            Err(EngineError::SnapshotNotFound(id)) => assert_eq!(id, "missing"),
            other => panic!("expected SnapshotNotFound, got {other:?}"),
        }
        assert!(
            !driver
                .journal()
                .iter()
                .any(|op| matches!(op, DriverOp::SnapshotWritable { .. })),
            "no world subvolume is created for an unknown snapshot"
        );
    }

    #[test]
    fn world_state_categories_are_plain_directories_inside_the_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let world_dir = engine.world_dir("slot-1-1");
        fs::create_dir_all(&world_dir).expect("world subvolume");
        let rootfs = rootfs(&engine);
        let app_rootfs = rootfs.app_rootfs.clone();
        fs::create_dir_all(&app_rootfs).expect("app rootfs");
        // A clone of a sealed world inherits a stale reference; materialization repoints it.
        std::os::unix::fs::symlink(
            tmp.path().join("cache/rootfs/stale"),
            world_dir.join(WORLD_ROOTFS_DIR),
        )
        .expect("stale rootfs reference");
        let spec = WorldSpec {
            target: WorldTarget::TestSlot(1),
            rootfs,
            snapshot: SnapshotId("base".to_owned()),
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(tmp.path().join("home")),
            local_ingress: None,
            boot: BootLineage::default(),
        };

        engine
            .materialize_world(&world_dir, &spec)
            .expect("materialize world layout");

        let rootfs_link = world_dir.join(WORLD_ROOTFS_DIR);
        assert_eq!(
            fs::read_link(&rootfs_link).expect("rootfs is a read-only reference"),
            app_rootfs,
            "the content-addressed cache is referenced, never copied or mutated"
        );
        for category in WORLD_STATE_CATEGORIES {
            let dir = world_dir.join(WORLD_STATE_DIR).join(category);
            assert!(dir.is_dir(), "{} must be a plain directory", dir.display());
        }
        assert!(
            driver
                .list_children(&world_dir)
                .expect("list nested subvolumes")
                .is_empty(),
            "a world subvolume never contains a nested subvolume"
        );
        // Idempotent: a clone of a sealed world already carries its layout.
        engine
            .materialize_world(&world_dir, &spec)
            .expect("materialization is idempotent");
    }

    #[test]
    fn a_rootfs_outside_the_content_addressed_cache_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let source = tmp.path().join("base-seed");
        fs::create_dir_all(&source).expect("source");
        engine
            .create_snapshot(&source, &SnapshotId("base".to_owned()))
            .expect("base snapshot");
        let mut foreign = rootfs(&engine);
        foreign.app_rootfs = tmp.path().join("elsewhere/app");
        let spec = WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: foreign.clone(),
            snapshot: SnapshotId("base".to_owned()),
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(tmp.path().join("home")),
            local_ingress: None,
            boot: BootLineage::default(),
        };
        driver.clear_journal();

        match engine.clone_world(&spec) {
            Err(EngineError::InvalidWorldSpec { reason }) => {
                assert!(reason.contains("content-addressed cache"), "{reason}");
                assert!(reason.contains("elsewhere/app"), "{reason}");
            }
            other => panic!("a foreign rootfs must fail closed: {other:?}"),
        }
        assert!(
            !driver
                .journal()
                .iter()
                .any(|op| matches!(op, DriverOp::SnapshotWritable { .. })),
            "no world subvolume is created for a foreign rootfs: {:?}",
            driver.journal()
        );

        let world_dir = engine.world_dir("slot-0-1");
        fs::create_dir_all(&world_dir).expect("world subvolume");
        let materialize_spec = WorldSpec {
            rootfs: foreign,
            ..spec
        };
        match engine.materialize_world(&world_dir, &materialize_spec) {
            Err(EngineError::InvalidWorldSpec { reason }) => {
                assert!(reason.contains("content-addressed cache"), "{reason}");
            }
            other => panic!("materialize_world must refuse a foreign rootfs: {other:?}"),
        }
        assert!(
            !world_dir.join(WORLD_ROOTFS_DIR).exists(),
            "a refused rootfs materializes nothing"
        );
    }

    #[test]
    fn a_failed_teardown_is_retryable_and_never_leaks_the_world() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs().with_destroy_failures(1));
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let launcher = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .spawn()
            .expect("world init launcher");
        let init_pid =
            wait_for_inner_pid(launcher.id(), Duration::from_secs(5)).expect("world init pid");
        let mut record = world_record(&engine, "slot-0-7", Some((launcher, init_pid)));
        let (state, _inner) = running_world_process(&mut record, "api", Instant::now());
        let ctl_dir = record.ctl_dir.clone();
        let world_dir = record.world_dir.clone();
        fs::write(ctl_dir.join(WORLD_INIT_CONFIG_FILE), b"{}").expect("control marker");
        let world = insert(&engine, record);

        match engine.destroy_world(world.clone()) {
            Err(EngineError::Ioctl {
                op: "snap_destroy", ..
            }) => {}
            other => panic!("the first teardown must surface the driver failure: {other:?}"),
        }
        assert_eq!(
            state.load(Ordering::Relaxed),
            ServiceState::Stopped.to_bits(),
            "the first attempt stopped the service"
        );
        assert!(
            !ctl_dir.exists(),
            "the first attempt released the control directory"
        );
        assert!(
            matches!(
                engine.freeze_world(&world, &SnapshotId("x".to_owned())),
                Err(EngineError::WorldGone { .. })
            ),
            "a world whose teardown started is gone for every caller"
        );

        engine
            .destroy_world(world.clone())
            .expect("the retried teardown completes");
        assert!(!world_dir.exists(), "the world subvolume is destroyed");
        assert!(
            matches!(
                engine.destroy_world(world),
                Err(EngineError::WorldGone { .. })
            ),
            "the record is gone after a successful teardown"
        );
    }

    #[test]
    fn a_snapshot_subvolume_source_is_sealed_without_copying() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let sealed = engine
            .driver
            .create_subvolume(&engine.snapshots_dir(), "sealed-base")
            .expect("sealed base subvolume");
        driver.clear_journal();

        engine
            .create_snapshot(&sealed, &SnapshotId("copy-of-base".to_owned()))
            .expect("a snapshot subvolume is sealed directly");

        let journal = driver.journal();
        assert!(
            !journal
                .iter()
                .any(|op| matches!(op, DriverOp::CreateSubvolume { .. })),
            "a managed snapshot subvolume is never copied: {journal:?}"
        );
        assert!(
            journal.iter().any(|op| matches!(
                op,
                DriverOp::SnapshotRo { source, dest, .. }
                    if source == &sealed && dest == &tmp.path().join("snapshots/copy-of-base")
            )),
            "the snapshot subvolume is sealed directly: {journal:?}"
        );
    }

    #[test]
    fn an_unmanaged_directory_under_snapshots_is_refused_as_a_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let foreign = engine.snapshots_dir().join("not-a-subvolume");
        fs::create_dir_all(&foreign).expect("foreign snapshot directory");

        match engine.create_snapshot(&foreign, &SnapshotId("foreign".to_owned())) {
            Err(EngineError::Io { path, message }) => {
                assert_eq!(path, foreign.display().to_string());
                assert!(message.contains("not a managed subvolume"), "{message}");
                assert!(message.contains("base-seed directory"), "{message}");
            }
            other => panic!("an unmanaged snapshot source must fail closed: {other:?}"),
        }
    }

    #[test]
    fn sealing_an_existing_snapshot_id_fails_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let source = tmp.path().join("base-seed");
        fs::create_dir_all(&source).expect("source");
        engine
            .create_snapshot(&source, &SnapshotId("taken".to_owned()))
            .expect("first seal");

        match engine.create_snapshot(&source, &SnapshotId("taken".to_owned())) {
            Err(EngineError::SnapshotExists { id }) => assert_eq!(id, "taken"),
            other => panic!("a duplicate snapshot id must fail closed: {other:?}"),
        }
        assert!(
            !driver.journal().iter().any(|op| matches!(
                op,
                DriverOp::DestroySubvolume { path, .. }
                    if path == &tmp.path().join("snapshots/taken")
            )),
            "the existing snapshot is never replaced: {:?}",
            driver.journal()
        );

        let mut record = world_record(&engine, "slot-0-8", None);
        let _ = running_world_process(&mut record, "api", Instant::now());
        let world = insert(&engine, record);
        match engine.freeze_world(&world, &SnapshotId("taken".to_owned())) {
            Err(EngineError::SnapshotExists { id }) => assert_eq!(id, "taken"),
            other => panic!("freezing into an existing id must fail closed: {other:?}"),
        }
        engine
            .freeze_world(&world, &SnapshotId("fresh".to_owned()))
            .expect("a failed seal leaves the world freezable");
    }

    #[test]
    fn world_ids_skip_names_a_previous_daemon_left_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        driver
            .create_subvolume(&engine.worlds_dir(), "slot-0-1")
            .expect("leftover world subvolume");

        let (seq, world_id) = engine
            .allocate_world_id(&WorldTarget::TestSlot(0))
            .expect("allocate a free world id");
        assert_eq!(world_id, "slot-0-2");
        assert_eq!(seq, 2, "the sequence follows the free name");
    }

    #[test]
    fn service_spec_validation_rejects_empty_argv_before_spawning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = engine_at(tmp.path(), Arc::new(RecordingDriver::btrfs()));
        let world = insert(&engine, world_record(&engine, "slot-0-3", None));
        let spec = ServiceSpec {
            id: "api".to_owned(),
            kind: ServiceKind::BackingService,
            runtime: ServiceRuntime::OciContainer {
                image: BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                argv: Vec::new(),
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("pgdata/main"),
                    container_path: PathBuf::from("/var/lib/postgresql/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: ExecEnv::default(),
            readiness_port: None,
        };
        match engine.spawn_service(&world, &spec) {
            Err(EngineError::InvalidWorldSpec { reason }) => {
                assert!(reason.contains("argv"), "{reason}");
            }
            other => panic!("expected InvalidWorldSpec, got {other:?}"),
        }
        assert!(
            engine
                .service_logs(&world, "api")
                .is_err_and(|err| matches!(err, EngineError::UnknownService { .. })),
            "a rejected spawn registers no service"
        );
    }

    #[test]
    fn reap_world_init_closes_stdin_lifeline_without_stalling_on_sigterm_immune_init() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        // `trap "" TERM` reproduces kernel `SIGNAL_UNKILLABLE` on a child PID-namespace init:
        // `SIGTERM` is ignored, so teardown must wake the init via EOF on `launcher.stdin` and
        // complete immediately instead of stalling for `WORLD_STOP_TERM_GRACE` (5s).
        let launcher = Command::new("sh")
            .args([
                "-c",
                "exec 3<&0; trap '' TERM; (trap '' TERM; IFS= read -r _ <&3 || exit 0) & exec 3<&-; wait",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("lifeline launcher");
        let launcher_pid = launcher.id();
        let init_pid =
            wait_for_inner_pid(launcher_pid, Duration::from_secs(5)).expect("lifeline init pid");
        let record = world_record(&engine, "slot-0-lifeline", Some((launcher, init_pid)));
        let world = insert(&engine, record);

        let started = Instant::now();
        engine.destroy_world(world).expect("destroy lifeline world");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "lifeline EOF teardown must not stall on WORLD_STOP_TERM_GRACE: {elapsed:?}"
        );
        assert!(
            !Path::new(&format!("/proc/{init_pid}")).exists(),
            "init_pid must be reaped"
        );
        assert!(
            !Path::new(&format!("/proc/{launcher_pid}")).exists(),
            "launcher must be reaped"
        );
    }

    #[test]
    fn a_failed_service_stop_keeps_the_service_running_so_a_retried_teardown_reaps_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let launcher = Command::new("sh")
            .args(["-c", "sleep 60 & wait"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("world launcher");
        let init_pid =
            wait_for_inner_pid(launcher.id(), Duration::from_secs(5)).expect("world init pid");
        let mut record = world_record(&engine, "slot-0-stop-retry", Some((launcher, init_pid)));
        let (state, inner_pid) = running_world_process(&mut record, "api", Instant::now());
        let log = record
            .services
            .get("api")
            .expect("recorded service")
            .log
            .clone();
        let service_process = Arc::clone(&record.services["api"].process);
        let inner = ProcessIdentity::capture(inner_pid).expect("service identity");
        let world = insert(&engine, record);

        // Inject a failed `kill_and_reap_child` at the exact decision seam production uses. A real
        // `Child::kill`/`try_wait` failure cannot be provoked deterministically from a live child
        // (SIGKILL always terminates it), so the failure is supplied as the outcome the engine's own
        // `finish_service_stop` receives when the bounded reap fails.
        let failed = finish_service_stop(
            &state,
            &log,
            Err(EngineError::Execution(format!(
                "service 'api' did not exit within {}s of SIGKILL",
                SERVICE_STOP_GRACE.as_secs()
            ))),
        );
        assert!(failed.is_err(), "a failed stop must report the error");
        assert_eq!(
            state.load(Ordering::Relaxed),
            ServiceState::Running.to_bits(),
            "a failed stop must leave the service Running so a retry re-enters the stop path"
        );
        assert!(
            !log.is_closed(),
            "a failed stop must keep the captured log open"
        );
        assert!(
            service_process
                .lock()
                .expect("service process")
                .supervisor
                .is_some(),
            "the un-reaped supervisor must stay recorded for the retry"
        );

        // The retried teardown short-circuits nothing: it signals and reaps the supervisor.
        engine
            .destroy_world(world)
            .expect("a retried teardown reaps the surviving supervisor");
        assert_eq!(
            state.load(Ordering::Relaxed),
            ServiceState::Stopped.to_bits(),
            "the retried stop reports Stopped"
        );
        assert!(
            inner.wait_gone(Duration::from_secs(5)),
            "the retried teardown must reap the service process"
        );
        assert!(log.is_closed(), "a successful retry closes the log");
    }

    #[test]
    fn reap_world_init_escalates_to_sigkill_and_reaps_launcher_when_init_ignores_eof_and_sigterm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver) as Arc<RecordingDriver>);
        let launcher = Command::new("sh")
            .args([
                "-c",
                "trap '' TERM; (trap '' TERM; sleep 60) </dev/null & wait",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("wedged launcher");
        let launcher_pid = launcher.id();
        let init_pid =
            wait_for_inner_pid(launcher_pid, Duration::from_secs(5)).expect("wedged init pid");
        let record = world_record(&engine, "slot-0-wedged", Some((launcher, init_pid)));
        let world = insert(&engine, record);

        engine
            .destroy_world(world)
            .expect("SIGKILL escalation reaps wedged world");

        assert!(
            !Path::new(&format!("/proc/{init_pid}")).exists(),
            "SIGKILL escalation must reap init_pid"
        );
        assert!(
            !Path::new(&format!("/proc/{launcher_pid}")).exists(),
            "SIGKILL escalation must reap launcher"
        );
    }

    #[test]
    fn dropping_the_engine_reaps_live_world_launchers_and_services() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = Arc::new(RecordingDriver::btrfs());
        let engine = engine_at(tmp.path(), Arc::clone(&driver));

        // 1. A raw `init_pid` lacking `/proc/<pid>/ns/pid` identity fails closed without signalling
        //    a potentially recycled PID, and a mismatched `/proc/<pid>/ns/pid` identity never kills
        //    a bystander process on either `destroy_world` or `Drop`.
        let mut bystander = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("bystander process");
        let bystander_pid = bystander.id();
        let mut unverified = world_record(&engine, "slot-0-unverified", None);
        unverified.init_pid = bystander_pid;
        let unverified_handle = insert(&engine, unverified);
        match engine.destroy_world(unverified_handle.clone()) {
            Err(EngineError::Execution(message)) => {
                assert!(message.contains("/proc/"), "{message}");
            }
            other => panic!("unverified raw init_pid must fail closed: {other:?}"),
        }
        assert!(
            bystander.try_wait().expect("observe bystander").is_none(),
            "an unverified raw init_pid must never signal the bystander"
        );
        engine
            .lock()
            .expect("world registry")
            .remove(&unverified_handle.seq());

        let mut recycled = world_record(&engine, "slot-0-recycled", None);
        recycled.init_pid = bystander_pid;
        recycled.init_identity = Some(ProcessIdentity {
            pid: bystander_pid,
            pid_ns: PathBuf::from("pid:[1]"),
            start_ticks: 0,
        });
        let _recycled_handle = insert(&engine, recycled);

        // 2. A live leaked world has its launcher + init + service reaped AND its control dir +
        //    world subvolume reclaimed via the shared teardown path when `BtrfsEngine` drops.
        let launcher = Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 60 & wait"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("world launcher");
        let launcher_pid = launcher.id();
        let init_pid =
            wait_for_inner_pid(launcher_pid, Duration::from_secs(5)).expect("world init pid");
        let launcher_identity = ProcessIdentity::capture(launcher_pid).expect("launcher identity");
        let init_identity = ProcessIdentity::capture(init_pid).expect("init identity");
        let mut record = world_record(&engine, "slot-0-drop", Some((launcher, init_pid)));
        let ctl_dir = record.ctl_dir.clone();
        let world_dir = record.world_dir.clone();
        fs::write(ctl_dir.join(WORLD_INIT_CONFIG_FILE), b"{}").expect("control marker");
        let (_state, service_pid) = running_world_process(&mut record, "api", Instant::now());
        let service_identity = ProcessIdentity::capture(service_pid).expect("service identity");
        let _world = insert(&engine, record);
        driver.clear_journal();

        drop(engine);

        for identity in [launcher_identity, init_identity, service_identity] {
            assert!(
                identity.wait_gone(Duration::from_secs(5)),
                "Drop for BtrfsEngine must kill and reap pid {}",
                identity.pid
            );
        }
        assert!(
            !ctl_dir.exists() && !world_dir.exists(),
            "Drop must reclaim both the control directory and the world subvolume"
        );
        assert!(
            driver.journal().iter().any(|op| matches!(
                op,
                DriverOp::DestroySubvolume { path, .. } if path == &world_dir
            )),
            "Drop must destroy the leaked world subvolume through the driver: {:?}",
            driver.journal()
        );
        assert!(
            bystander.try_wait().expect("observe bystander").is_none(),
            "Drop must never signal a PID whose /proc/<pid>/ns/pid does not match"
        );
        let _ = bystander.kill();
        let _ = bystander.wait();
    }
}
