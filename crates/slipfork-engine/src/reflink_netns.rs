//! Rootless Linux `FICLONE` + persistent per-world PID 1 implementation of [`WorldEngine`]
//! (`R10`, `R11`, `ADR-0002`, `H-011`).
//!
//! World state is cloned with real `ioctl(FICLONE)` reflinks. Every world owns a **persistent**
//! `slipfork-world-init` PID 1 (`src/bin/slipfork-world-init.rs`) spawned inside
//! `unshare --map-user=$UID --map-group=$GID --keep-caps --mount --net --pid --fork --mount-proc`:
//! it establishes the world's bind mounts once, answers `dial_world_tcp` over
//! `<world_dir>/netns.sock` (`SCM_RIGHTS`), and optionally bridges `127.0.0.1:<port>` inside the
//! world to `slipforkd`'s `AF_UNIX` HTTP ingress (`LocalIngressBridge`).
//!
//! A daemon thread can never `setns` into an unprivileged world `netns` (`EPERM`) and
//! `setns(CLONE_NEWUSER)` is `EINVAL` in a multithreaded process, so namespace-scoped capabilities
//! live in the world's own PID 1 while the engine enters from single-threaded `nsenter` children
//! (`exec_in_world`, `spawn_service`, `entry_command`). Every process keeps the exact host
//! `$UID:$GID`. No `unsafe` code lives here: the only syscall module is [`crate::linux_cow`].

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::WorldTarget;

use crate::child_env;
use crate::linux_cow::{self, ProcessSignal, reflink_tree};
use crate::oci::{self, ContainerBundle, ContainerMount};
use crate::rootfs::{OciRootfs, WorldRootfs};
use crate::service::ServiceLogs;
use crate::world_init::{
    WORLD_INIT_CONFIG_FILE, WORLD_INIT_SOCKET_FILE, WorldInitConfig, WorldInitIngress,
    WorldInitMount, WorldInitRootfs,
};
use crate::{
    EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServiceRuntime, ServiceSpec,
    ServiceState, SnapshotId, StateSubdirMount, StreamChunk, StreamSource, WorldEngine,
    WorldEntryCommand, WorldHandle, WorldMount, WorldSpec, WorldStatus, target_slug,
};

/// Installed file name of the per-world PID 1 helper.
const WORLD_INIT_PROGRAM: &str = "slipfork-world-init";
/// Fixed search path for helper subprocesses (`ip`, `mount`, `nsenter`); nothing is inherited.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// Guest mount of the world's CoW `state_dir`.
const WORLD_STATE_GUEST: &str = "/.slipfork/state";
/// Guest mount of the host `<world_dir>/oci` bundle directory.
const OCI_BUNDLES_GUEST: &str = "/.slipfork/oci";
/// Guest mount of the host `<world_dir>/oci-run` `crun` state root.
const OCI_RUN_GUEST: &str = "/.slipfork/run";
/// Guest mount prefix of extracted backing-service OCI rootfs trees.
const OCI_IMAGES_GUEST: &str = "/.slipfork/images";
/// Guest path of the synthesized `/etc/passwd`.
const PASSWD_GUEST: &str = "/.slipfork/etc/passwd";
/// Guest path of the synthesized `/etc/group`.
const GROUP_GUEST: &str = "/.slipfork/etc/group";
/// Guest path of `slipforkd`'s ingress socket after `pivot_root`.
const INGRESS_GUEST: &str = "/.slipfork/http-ingress.sock";
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
/// Grace period for a world PID namespace to disappear after `SIGKILL`.
const WORLD_STOP_GRACE: Duration = Duration::from_secs(10);
/// Poll interval for bounded waits.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Unprivileged Linux CoW world engine backed by `ioctl(FICLONE)` + a persistent world PID 1.
#[derive(Debug, Clone)]
pub struct ReflinkNetnsEngine {
    state_root: PathBuf,
    host_uid: u32,
    host_gid: u32,
    world_init: WorldInitLauncher,
    shared: Arc<Shared>,
}

/// Where the per-world PID 1 helper executable comes from.
#[derive(Debug, Clone)]
enum WorldInitLauncher {
    /// Installed next to the running engine executable (or explicitly injected).
    Installed(PathBuf),
    /// Resolution failed; every `clone_world` fails closed with the recorded reason.
    Unresolved(String),
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

#[derive(Debug)]
struct WorldRecord {
    world_dir: PathBuf,
    state_dir: PathBuf,
    netns_sock: PathBuf,
    init_pid: u32,
    launcher: Option<Child>,
    base_env: ExecEnv,
    rootfs: WorldRootfs,
    mounts: Vec<WorldMount>,
    status: WorldStatus,
    services: BTreeMap<String, ServiceRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceKind {
    WorldProcess,
    OciContainer,
}

#[derive(Debug)]
struct ServiceRecord {
    kind: ServiceKind,
    log: ServiceLogs,
    state: Arc<AtomicU8>,
    process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
struct ServiceProcess {
    supervisor: Option<Child>,
    inner_pid: Option<u32>,
    container: Option<ContainerProcess>,
}

/// Everything a spawn path needs from the already-locked world record.
struct ServiceSpawn<'a> {
    world: &'a WorldHandle,
    init_pid: u32,
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

impl ReflinkNetnsEngine {
    /// Construct a `ReflinkNetnsEngine` storing snapshots and cloned state under `state_root`
    /// (which MUST reside on a reflink-capable filesystem such as XFS `reflink=1` or Btrfs).
    ///
    /// The `slipfork-world-init` helper is expected next to the running engine executable (both
    /// binaries ship together). Hosts that install it elsewhere, and tests, inject the absolute
    /// path with [`ReflinkNetnsEngine::with_world_init_program`] instead.
    pub fn new(state_root: impl AsRef<Path>, host_uid: u32, host_gid: u32) -> Self {
        let world_init = match std::env::current_exe() {
            Ok(executable) => match executable.parent() {
                Some(dir) => WorldInitLauncher::Installed(dir.join(WORLD_INIT_PROGRAM)),
                None => WorldInitLauncher::Unresolved(format!(
                    "engine executable {} has no parent directory",
                    executable.display()
                )),
            },
            Err(err) => WorldInitLauncher::Unresolved(format!(
                "cannot resolve the engine executable path: {err}"
            )),
        };
        Self::with_launcher(state_root, host_uid, host_gid, world_init)
    }

    /// Construct a `ReflinkNetnsEngine` whose per-world PID 1 helper lives at `program`.
    ///
    /// This is the dependency-injection seam for the daemon composition root and for hosts where
    /// the helper is not co-located with the engine executable.
    pub fn with_world_init_program(
        state_root: impl AsRef<Path>,
        host_uid: u32,
        host_gid: u32,
        program: impl Into<PathBuf>,
    ) -> Self {
        Self::with_launcher(
            state_root,
            host_uid,
            host_gid,
            WorldInitLauncher::Installed(program.into()),
        )
    }

    fn with_launcher(
        state_root: impl AsRef<Path>,
        host_uid: u32,
        host_gid: u32,
        world_init: WorldInitLauncher,
    ) -> Self {
        Self {
            state_root: state_root.as_ref().to_path_buf(),
            host_uid,
            host_gid,
            world_init,
            shared: Arc::new(Shared::default()),
        }
    }

    fn snapshot_dir(&self, id: &SnapshotId) -> Result<PathBuf, EngineError> {
        Ok(self.state_root.join("snapshots").join(id.validate()?))
    }

    fn world_dir(&self, target: &WorldTarget, seq: u64) -> PathBuf {
        self.state_root
            .join("worlds")
            .join(format!("{}-{seq}", target_slug(target)))
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<u64, WorldRecord>>, EngineError> {
        self.shared.worlds.lock().map_err(|_| {
            EngineError::Execution("reflink-netns world registry lock is poisoned".to_owned())
        })
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

    fn world_init_program(&self, world: &str) -> Result<&Path, EngineError> {
        match &self.world_init {
            WorldInitLauncher::Installed(program) => Ok(program),
            WorldInitLauncher::Unresolved(reason) => Err(EngineError::WorldInitFailed {
                world: world.to_owned(),
                reason: format!(
                    "cannot locate `{WORLD_INIT_PROGRAM}` ({reason}); construct the engine with \
                     ReflinkNetnsEngine::with_world_init_program(<absolute path>)"
                ),
            }),
        }
    }

    /// `nsenter` arguments entering every namespace of `init_pid`.
    ///
    /// The working directory is applied by [`world_cwd_wrapper`] **after** the mount-namespace
    /// entry: a path that lives on a bind mount (the repo, `<state_dir>`) has no reachable host
    /// pathname after `pivot_root`, so entering `--wd=<host path>` leaves `getcwd(2)` failing.
    fn entry_prefix(&self, init_pid: u32, cwd: &Path) -> Vec<String> {
        vec![
            format!("--target={init_pid}"),
            "--user".to_owned(),
            "--preserve-credentials".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--pid".to_owned(),
            format!("--wd={}", cwd.display()),
            "--".to_owned(),
        ]
    }

    /// Roll a failed seal back to `Live` so a transient filesystem error does not brick the world.
    fn reopen_after_failed_freeze(&self, world: &WorldHandle) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
            && record.status == WorldStatus::Freezing
        {
            record.status = WorldStatus::Live;
        }
    }

    /// Materialize the world directory's init configuration, boot the PID 1, and wait until its
    /// listeners are ready. Returns the dial socket path, the PID 1 host pid, and its launcher.
    fn boot_world(
        &self,
        label: &str,
        world_root: &Path,
        state_dir: &Path,
        spec: &WorldSpec,
    ) -> Result<(PathBuf, u32, Child), EngineError> {
        let program = self.world_init_program(label)?.to_path_buf();
        let netns_sock = world_root.join(WORLD_INIT_SOCKET_FILE);
        if !crate::world_init::unix_socket_path_fits(&netns_sock) {
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
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        let crun_guest = spec.rootfs.crun_bin.clone();
        let crun_host = spec
            .rootfs
            .app_rootfs
            .join(crun_guest.strip_prefix("/").unwrap_or(&crun_guest));
        if !crun_host.is_file() {
            return Err(EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "the world rootfs '{}' does not contain the pinned static crun at \
                     '{}' ({})",
                    spec.rootfs.app_rootfs.display(),
                    crun_guest.display(),
                    crun_host.display()
                ),
            });
        }
        let mountpoints = world_root.join("mountpoints");
        let merged = world_root.join("merged");
        let oci_dir = world_root.join("oci");
        let oci_run = world_root.join("oci-run");
        for dir in [&mountpoints, &merged, &oci_dir, &oci_run] {
            fs::create_dir_all(dir).map_err(|err| EngineError::Io {
                path: dir.display().to_string(),
                message: err.to_string(),
            })?;
        }
        let (passwd, group) = synthesize_identity_files(
            world_root,
            &spec.rootfs,
            &home,
            self.host_uid,
            self.host_gid,
        )?;

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
            host_path: state_dir.to_path_buf(),
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
                app_rootfs: spec.rootfs.app_rootfs.clone(),
                mountpoints,
                merged,
            },
            mounts,
            local_ingress,
        };
        let config_path = world_root.join(WORLD_INIT_CONFIG_FILE);
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

        // Identity UID/GID mapping (`$UID -> $UID`) + `--keep-caps` preserves exact `$UID:$GID`
        // ownership on every file written to a shared mount and grants `CAP_SYS_ADMIN` /
        // `CAP_NET_ADMIN` / `CAP_NET_BIND_SERVICE` inside the user namespace.
        let mut command = Command::new("unshare");
        command
            .arg(format!("--map-user={}", self.host_uid))
            .arg(format!("--map-group={}", self.host_gid))
            .args([
                "--keep-caps",
                "--mount",
                "--net",
                "--pid",
                "--fork",
                "--mount-proc",
            ])
            .arg(&program)
            .arg(&config_path)
            .env_clear()
            .env("PATH", HELPER_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut launcher = command
            .spawn()
            .map_err(|err| EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!("cannot spawn `unshare` for the world init: {err}"),
            })?;

        let deadline = Instant::now() + WORLD_INIT_TIMEOUT;
        let mut init_pid = None;
        let outcome: Result<u32, String> = loop {
            if init_pid.is_none() {
                match host_child_pids(launcher.id()) {
                    Ok(children) => init_pid = children.first().copied(),
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
            if let Some(pid) = init_pid
                && fs::read_to_string(config.ready_path()).is_ok_and(|ready| ready == "ready\n")
                && netns_sock.exists()
            {
                break Ok(pid);
            }
            match launcher.try_wait() {
                Ok(Some(status)) => {
                    break Err(format!(
                        "the world init exited with {status} before becoming ready"
                    ));
                }
                Ok(None) => {}
                Err(err) => break Err(format!("cannot observe the world init process: {err}")),
            }
            if Instant::now() >= deadline {
                break Err(format!(
                    "timed out after {}s waiting for {} (init pid {init_pid:?})",
                    WORLD_INIT_TIMEOUT.as_secs(),
                    config.ready_path().display()
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };

        match outcome {
            Ok(init_pid) => Ok((netns_sock, init_pid, launcher)),
            Err(reason) => {
                if let Some(pid) = init_pid {
                    let _ = linux_cow::signal_process(pid, ProcessSignal::Kill);
                }
                let _ = launcher.kill();
                let stderr = drain_stderr(&mut launcher);
                let _ = launcher.wait();
                Err(EngineError::WorldInitFailed {
                    world: label.to_owned(),
                    reason: format!("{reason}; helper stderr: {stderr}"),
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
            spawn_log_reader(stdout, StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, StreamSource::Stderr, spawn.log.clone());
        }
        let supervisor_pid = child.id();
        if let Ok(mut process) = spawn.process.lock() {
            process.supervisor = Some(child);
        }
        // `nsenter --pid` forks: the service itself is the launcher's first child. `SIGTERM` must
        // reach that in-world pid, never the launcher parent.
        if let Some(inner_pid) = wait_for_inner_pid(supervisor_pid, SERVICE_STOP_GRACE)
            && let Ok(mut process) = spawn.process.lock()
        {
            process.inner_pid = Some(inner_pid);
        }
        Ok(())
    }

    /// Start one daemonless `crun` backing-service container (`ServiceRuntime::OciContainer`).
    fn spawn_oci_container(
        &self,
        spawn: &ServiceSpawn<'_>,
        image: &crate::BackingServiceKey,
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
        let bundle_host = spawn.state_dir.parent().map_or_else(
            || spawn.state_dir.join("oci").join(&container_id),
            |world_dir| world_dir.join("oci").join(&container_id),
        );
        fs::create_dir_all(&bundle_host).map_err(|err| EngineError::Io {
            path: bundle_host.display().to_string(),
            message: err.to_string(),
        })?;
        let bundle_guest = PathBuf::from(OCI_BUNDLES_GUEST).join(&container_id);

        let mut mounts = Vec::with_capacity(state_mounts.len() + readonly_mounts.len());
        for state in state_mounts {
            oci::validate_state_subdir(&state.host_subdir)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            oci::validate_container_path(&state.container_path)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            let host = spawn.state_dir.join(&state.host_subdir);
            fs::create_dir_all(&host).map_err(|err| EngineError::Io {
                path: host.display().to_string(),
                message: err.to_string(),
            })?;
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
        .map_err(|err| EngineError::Io {
            path: bundle_host.display().to_string(),
            message: err.to_string(),
        })?;

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
            spawn_log_reader(stdout, StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, StreamSource::Stderr, spawn.log.clone());
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

    /// Build a `crun` invocation that runs inside the world's mount/net/pid namespaces.
    fn crun_command(
        &self,
        init_pid: u32,
        crun_guest: &Path,
        crun_root_guest: &Path,
        env: &BTreeMap<String, String>,
    ) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, Path::new("/")))
            .arg(crun_guest)
            .args(["--cgroup-manager=disabled", "--root"])
            .arg(crun_root_guest)
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
        let record = self.owned(&mut worlds, service.world())?;
        if record.status == WorldStatus::Destroyed {
            return Err(EngineError::WorldGone {
                world: format!("{:?}", service.world()),
            });
        }
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

    /// Stop every live service of `world` before its state is sealed or torn down.
    fn stop_live_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let running: Vec<(String, ServiceKind)> = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            record
                .services
                .iter()
                .filter(|(_, service)| {
                    ServiceState::from_bits(service.state.load(Ordering::Relaxed))
                        == ServiceState::Running
                })
                .map(|(id, service)| (id.clone(), service.kind))
                .collect()
        };
        // `ADR-0009` §3: app components stop first, then backing-service containers, so no app
        // request can observe a half-shut-down database during the freeze.
        for kind in [ServiceKind::WorldProcess, ServiceKind::OciContainer] {
            for (id, running_kind) in &running {
                if *running_kind == kind {
                    self.stop_service_inner(world, id)?;
                }
            }
        }
        Ok(())
    }

    fn stop_service_inner(&self, world: &WorldHandle, id: &str) -> Result<(), EngineError> {
        let (state, log, process) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
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

        // Idempotent: a service that already exited on its own, or was stopped before, keeps its
        // retained logs and reports success.
        if ServiceState::from_bits(state.load(Ordering::Relaxed)) != ServiceState::Running {
            log.close();
            return Ok(());
        }

        let (supervisor, inner_pid, container) = {
            let Ok(mut process) = process.lock() else {
                return Err(EngineError::Execution(format!(
                    "service '{id}' process state is poisoned"
                )));
            };
            (
                process.supervisor.take(),
                process.inner_pid,
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
        let inner_pid = inner_pid.or_else(|| {
            host_child_pids(supervisor.id())
                .ok()
                .and_then(|children| children.first().copied())
        });

        if let Some(container) = &container {
            signal_container(self, container, "TERM");
        } else if let Some(pid) = inner_pid {
            let _ = linux_cow::signal_process(pid, ProcessSignal::Terminate);
        }
        if !wait_for_exit(&mut supervisor, SERVICE_STOP_GRACE) {
            if let Some(container) = &container {
                signal_container(self, container, "KILL");
            } else if let Some(pid) = inner_pid {
                let _ = linux_cow::signal_process(pid, ProcessSignal::Kill);
            }
            let _ = supervisor.kill();
            if !wait_for_exit(&mut supervisor, SERVICE_STOP_GRACE) {
                state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
                log.close();
                return Err(EngineError::Execution(format!(
                    "service '{id}' did not exit within {}s of SIGKILL",
                    SERVICE_STOP_GRACE.as_secs()
                )));
            }
        }
        if let Some(container) = &container {
            delete_container(self, container);
            let _ = fs::remove_dir_all(&container.bundle_host);
        }
        state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
        log.close();
        Ok(())
    }
}

impl WorldEngine for ReflinkNetnsEngine {
    fn name(&self) -> &'static str {
        "reflink-netns"
    }

    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
        let dst = self.snapshot_dir(id)?;
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| EngineError::Io {
                path: dst.display().to_string(),
                message: e.to_string(),
            })?;
        }
        reflink_tree(source_dir, &dst)?;
        Ok(())
    }

    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError> {
        let dst = self.snapshot_dir(id)?;

        // Services are stopped while the world is still live so the sealed snapshot is
        // crash-consistent (no process is mid-write while `state_dir` is reflinked).
        {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            match record.status {
                WorldStatus::Live => {}
                WorldStatus::Destroyed => {
                    return Err(EngineError::WorldGone {
                        world: format!("{world:?}"),
                    });
                }
                WorldStatus::Freezing | WorldStatus::Frozen => {
                    return Err(EngineError::WorldNotFreezable {
                        world: format!("{world:?}"),
                    });
                }
            }
        }
        self.stop_live_services(world)?;

        let source = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            match record.status {
                WorldStatus::Live => {
                    record.status = WorldStatus::Freezing;
                    record.state_dir.clone()
                }
                WorldStatus::Destroyed => {
                    return Err(EngineError::WorldGone {
                        world: format!("{world:?}"),
                    });
                }
                WorldStatus::Freezing | WorldStatus::Frozen => {
                    return Err(EngineError::WorldNotFreezable {
                        world: format!("{world:?}"),
                    });
                }
            }
        };

        let sealed = (|| {
            if dst.exists() {
                fs::remove_dir_all(&dst).map_err(|err| EngineError::Io {
                    path: dst.display().to_string(),
                    message: err.to_string(),
                })?;
            }
            reflink_tree(&source, &dst).map(|_| ())
        })();
        if let Err(err) = sealed {
            self.reopen_after_failed_freeze(world);
            return Err(err);
        }

        let sealed = {
            let mut worlds = self.lock()?;
            match self.owned(&mut worlds, world) {
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
            // The world was destroyed while the reflink was running: drop the half-claimed
            // snapshot and fail closed instead of resurrecting a destroyed world.
            let _ = fs::remove_dir_all(&dst);
            return Err(err);
        }
        Ok(())
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        spec.validate()?;
        let src = self.snapshot_dir(&spec.snapshot)?;
        if !src.is_dir() {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        fs::create_dir_all(&home).map_err(|e| EngineError::Io {
            path: home.display().to_string(),
            message: e.to_string(),
        })?;

        let seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let world = self.mint_world_handle(seq);
        let label = format!("{}#{seq}", target_slug(&spec.target));
        let world_root = self.world_dir(&spec.target, seq);
        let state_dir = world_root.join("state");
        if let Err(err) = reflink_tree(&src, &state_dir) {
            let _ = fs::remove_dir_all(&world_root);
            return Err(err);
        }
        let booted = self.boot_world(&label, &world_root, &state_dir, spec);
        let (netns_sock, init_pid, launcher) = match booted {
            Ok(booted) => booted,
            Err(err) => {
                let _ = fs::remove_dir_all(&world_root);
                return Err(err);
            }
        };

        self.lock()?.insert(
            seq,
            WorldRecord {
                world_dir: world_root,
                state_dir,
                netns_sock,
                init_pid,
                launcher: Some(launcher),
                base_env: spec.base_env.clone(),
                rootfs: spec.rootfs.clone(),
                mounts: spec.mounts.clone(),
                status: WorldStatus::Live,
                services: BTreeMap::new(),
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
        let kind = match spec.runtime {
            ServiceRuntime::WorldProcess { .. } => ServiceKind::WorldProcess,
            ServiceRuntime::OciContainer { .. } => ServiceKind::OciContainer,
        };

        let (init_pid, state_dir, rootfs, mounts, env, log, state, process) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
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
                    kind,
                    log: log.clone(),
                    state: Arc::clone(&state),
                    process: Arc::clone(&process),
                },
            );
            (
                init_pid, state_dir, rootfs, mounts, env, log, state, process,
            )
        };

        let spawn = ServiceSpawn {
            world,
            init_pid,
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
            let timeout = if kind == ServiceKind::OciContainer {
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
                        abort_service(self, &process, &state, &log);
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
                    abort_service(self, &process, &state, &log);
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
        let record = self.owned(&mut worlds, world)?;
        record
            .services
            .get(id)
            .map(|service| service.log.clone())
            .ok_or_else(|| EngineError::UnknownService {
                world: format!("{world:?}"),
                id: id.to_owned(),
            })
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
        let mut args_prefix = self.entry_prefix(container.init_pid, Path::new("/"));
        args_prefix.push(container.crun_guest.display().to_string());
        args_prefix.extend([
            "--cgroup-manager=disabled".to_owned(),
            "--root".to_owned(),
            container.crun_root_guest.display().to_string(),
            "exec".to_owned(),
            container.id,
            "--".to_owned(),
        ]);
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
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
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
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
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
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
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
        let (world_dir, init_pid, launcher, services) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, &world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            record.status = WorldStatus::Destroyed;
            let services: Vec<(Arc<AtomicU8>, ServiceLogs, Arc<Mutex<ServiceProcess>>)> = record
                .services
                .values()
                .map(|service| {
                    (
                        Arc::clone(&service.state),
                        service.log.clone(),
                        Arc::clone(&service.process),
                    )
                })
                .collect();
            (
                record.world_dir.clone(),
                record.init_pid,
                record.launcher.take(),
                services,
            )
        };

        for (state, log, _) in &services {
            state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
            log.close();
        }
        // `SIGKILL` to the world PID 1 makes the kernel destroy the whole PID namespace, so no
        // world-owned process (service, hook, or exec) can outlive its world.
        let _ = linux_cow::signal_process(init_pid, ProcessSignal::Kill);
        if let Some(mut launcher) = launcher {
            let _ = launcher.kill();
            let _ = launcher.wait();
        }
        if !wait_for_path_gone(init_pid, WORLD_STOP_GRACE) {
            return Err(EngineError::Execution(format!(
                "world init pid {init_pid} is still alive {}s after SIGKILL",
                WORLD_STOP_GRACE.as_secs()
            )));
        }
        for (_, _, process) in services {
            if let Ok(mut process) = process.lock()
                && let Some(mut supervisor) = process.supervisor.take()
            {
                let _ = supervisor.kill();
                let _ = supervisor.wait();
            }
        }
        fs::remove_dir_all(&world_dir).map_err(|err| EngineError::Io {
            path: world_dir.display().to_string(),
            message: err.to_string(),
        })?;
        // The world dir is gone, so the record must go too: a long-lived daemon creates and
        // destroys worlds forever, and the old handle now resolves to `EngineError::WorldGone`
        // exactly like every other operation on a destroyed world.
        self.lock()?.remove(&world.seq());
        Ok(())
    }
}

/// `sh -c 'cd -- "$1" && shift && exec "$@"' sh <cwd> [argv...]`.
///
/// The `cd` runs **inside** the entered mount namespace, so `getcwd(2)` resolves through the
/// world's bind mounts instead of the hidden host path.
fn world_cwd_wrapper(cwd: &Path, argv: &[String]) -> Vec<String> {
    let mut wrapped = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "cd -- \"$1\" && shift && exec \"$@\"".to_owned(),
        "sh".to_owned(),
        cwd.display().to_string(),
    ];
    wrapped.extend(argv.iter().cloned());
    wrapped
}

fn world_init_mount(mount: &WorldMount) -> WorldInitMount {
    WorldInitMount {
        host_path: mount.host_path.clone(),
        guest_path: mount.guest_path.clone(),
        read_only: mount.mode == MountMode::ReadOnly,
    }
}

/// Guest mount of one extracted OCI rootfs, keyed by its immutable image digest.
fn image_guest_rootfs(digest: &str) -> Result<PathBuf, EngineError> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| EngineError::InvalidWorldSpec {
            reason: format!(
                "backing-service image digest '{digest}' must be `sha256:<64 lowercase hex>`"
            ),
        })?;
    Ok(PathBuf::from(OCI_IMAGES_GUEST).join(hex).join("rootfs"))
}

/// Synthesize read-only `/etc/passwd` + `/etc/group` carrying the invoking `$UID:$GID`
/// (`ADR-0009` §3.1 rule 2): `initdb` fails with `could not look up effective user ID` without it.
fn synthesize_identity_files(
    world_root: &Path,
    rootfs: &WorldRootfs,
    home: &Path,
    uid: u32,
    gid: u32,
) -> Result<(PathBuf, PathBuf), EngineError> {
    let etc = world_root.join("etc");
    fs::create_dir_all(&etc).map_err(|err| EngineError::Io {
        path: etc.display().to_string(),
        message: err.to_string(),
    })?;
    let user = host_identity_name("/etc/passwd", uid, 2).unwrap_or_else(|| "slipfork".to_owned());
    let group = host_identity_name("/etc/group", gid, 2).unwrap_or_else(|| "slipfork".to_owned());

    let passwd = with_identity_line(
        crate::rootfs::read_guest_file(&rootfs.app_rootfs, "/etc/passwd"),
        uid,
        &format!(
            "{user}:x:{uid}:{gid}:slipfork world user:{}:/bin/sh",
            home.display()
        ),
    );
    let group_file = with_identity_line(
        crate::rootfs::read_guest_file(&rootfs.app_rootfs, "/etc/group"),
        gid,
        &format!("{group}:x:{gid}:"),
    );
    let passwd_path = etc.join("passwd");
    let group_path = etc.join("group");
    fs::write(&passwd_path, passwd).map_err(|err| EngineError::Io {
        path: passwd_path.display().to_string(),
        message: err.to_string(),
    })?;
    fs::write(&group_path, group_file).map_err(|err| EngineError::Io {
        path: group_path.display().to_string(),
        message: err.to_string(),
    })?;
    Ok((passwd_path, group_path))
}

fn host_identity_name(path: &str, id: u32, field: usize) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    raw.lines().find_map(|line| {
        let parts: Vec<&str> = line.split(':').collect();
        (parts.get(field)?.parse::<u32>().ok()? == id).then(|| parts[0].to_owned())
    })
}

fn with_identity_line(source: Option<Vec<u8>>, id: u32, line: &str) -> Vec<u8> {
    let mut bytes = source.unwrap_or_default();
    let present = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
        text.lines().any(|existing| {
            existing
                .split(':')
                .nth(2)
                .is_some_and(|field| field.parse::<u32>() == Ok(id))
        })
    });
    if !present {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn ensure_container_destination(
    image: &OciRootfs,
    image_key: &crate::BackingServiceKey,
    container_path: &Path,
) -> Result<(), EngineError> {
    let relative = container_path
        .strip_prefix("/")
        .map_err(|_| EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' must be absolute",
                container_path.display()
            ),
        })?;
    if !image.rootfs.join(relative).exists() {
        return Err(EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' does not exist in backing-service image {image_key}; \
                 the image rootfs must carry the mount destination",
                container_path.display()
            ),
        });
    }
    Ok(())
}

/// Translate one host path into the guest path the world sees after `pivot_root`.
fn world_guest_for_host(mounts: &[WorldMount], host_path: &Path) -> Option<PathBuf> {
    mounts.iter().find_map(|mount| {
        host_path
            .strip_prefix(&mount.host_path)
            .ok()
            .map(|relative| mount.guest_path.join(relative))
    })
}

/// Stable OCI container id derived from the world sequence and service id.
fn container_id(world_seq: u64, service_id: &str) -> String {
    let slug: String = service_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "service" } else { slug };
    format!("sf{world_seq}-{}", &slug[..slug.len().min(48)])
}

fn container_control(
    engine: &ReflinkNetnsEngine,
    container: &ContainerProcess,
    args: &[&str],
) -> Command {
    let mut command = Command::new("nsenter");
    command
        .args(engine.entry_prefix(container.init_pid, Path::new("/")))
        .arg(&container.crun_guest)
        .args(["--cgroup-manager=disabled", "--root"])
        .arg(&container.crun_root_guest)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn signal_container(engine: &ReflinkNetnsEngine, container: &ContainerProcess, signal: &str) {
    let _ = container_control(engine, container, &["kill", &container.id, signal]).status();
}

fn delete_container(engine: &ReflinkNetnsEngine, container: &ContainerProcess) {
    let _ = container_control(engine, container, &["delete", "--force", &container.id]).status();
}

fn run_streamed(
    mut command: Command,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    let mut child = command
        .spawn()
        .map_err(|err| EngineError::Execution(format!("cannot execute: {err}")))?;
    let (sender, receiver) = mpsc::channel::<StreamChunk>();
    let mut readers = Vec::with_capacity(2);
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(stdout, StreamSource::Stdout, sender.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(stderr, StreamSource::Stderr, sender.clone()));
    }
    drop(sender);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for chunk in receiver {
        match chunk.source {
            StreamSource::Stdout => stdout.extend_from_slice(&chunk.bytes),
            StreamSource::Stderr => stderr.extend_from_slice(&chunk.bytes),
        }
        on_output(chunk);
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child
        .wait()
        .map_err(|err| EngineError::Execution(err.to_string()))?;
    Ok(ExecOutput {
        exit_code: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut source: R,
    kind: StreamSource,
    sender: mpsc::Sender<StreamChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = source.read(&mut buffer) {
            if read == 0 {
                break;
            }
            if sender
                .send(StreamChunk {
                    source: kind,
                    bytes: buffer[..read].to_vec(),
                })
                .is_err()
            {
                break;
            }
        }
    })
}

fn spawn_log_reader<R: Read + Send + 'static>(mut source: R, kind: StreamSource, log: ServiceLogs) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = source.read(&mut buffer) {
            if read == 0 {
                break;
            }
            log.push(kind, buffer[..read].to_vec());
        }
    });
}

/// Kill and reap a service whose spawn never completed, then mark it `Stopped`.
///
/// Used when `spawn_service` fails after the process exists (readiness timeout or a world-level
/// error): leaving it alive would violate the typed failure and make a retry of the same id fail
/// with `ServiceAlreadyRunning`.
fn abort_service(
    engine: &ReflinkNetnsEngine,
    process: &Arc<Mutex<ServiceProcess>>,
    state: &Arc<AtomicU8>,
    log: &ServiceLogs,
) {
    let (supervisor, container) = process
        .lock()
        .map(|mut process| (process.supervisor.take(), process.container.clone()))
        .unwrap_or((None, None));
    if let Some(mut supervisor) = supervisor {
        if let Some(container) = &container {
            signal_container(engine, container, "KILL");
        } else if let Ok(children) = host_child_pids(supervisor.id())
            && let Some(inner_pid) = children.first()
        {
            let _ = linux_cow::signal_process(*inner_pid, ProcessSignal::Kill);
        }
        let _ = supervisor.kill();
        let _ = wait_for_exit(&mut supervisor, SERVICE_STOP_GRACE);
    }
    if let Some(container) = &container {
        delete_container(engine, container);
        let _ = fs::remove_dir_all(&container.bundle_host);
    }
    state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
    log.close();
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

/// Host pids of `pid`'s direct children, read from `/proc/<pid>/task/<pid>/children`.
fn host_child_pids(pid: u32) -> io::Result<Vec<u32>> {
    let raw = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    Ok(raw
        .split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect())
}

/// Wait (bounded) for `supervisor_pid`'s first child — the process `nsenter --pid` forked into the
/// world's PID namespace.
fn wait_for_inner_pid(supervisor_pid: u32, grace: Duration) -> Option<u32> {
    let deadline = Instant::now() + grace;
    loop {
        if let Ok(children) = host_child_pids(supervisor_pid)
            && let Some(pid) = children.first()
        {
            return Some(*pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Wait (bounded) for `child` to exit, reaping it.
fn wait_for_exit(child: &mut Child, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Wait (bounded) until `/proc/<pid>` disappears (the PID namespace is gone).
fn wait_for_path_gone(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Best-effort helper stderr tail for actionable boot diagnostics.
fn drain_stderr(child: &mut Child) -> String {
    let mut raw = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut raw);
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 2000 {
        trimmed.chars().take(2000).collect()
    } else {
        trimmed.to_owned()
    }
}
