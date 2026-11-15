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
use crate::service::ServiceLogs;
use crate::world_init::{
    WORLD_INIT_CONFIG_FILE, WORLD_INIT_SOCKET_FILE, WorldInitConfig, WorldInitIngress,
    WorldInitMount,
};
use crate::{
    EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServiceSpec, ServiceState,
    SnapshotId, SnapshotMode, StreamChunk, StreamSource, WorldEngine, WorldEntryCommand,
    WorldHandle, WorldSpec, WorldStatus, target_slug,
};

/// Installed file name of the per-world PID 1 helper.
const WORLD_INIT_PROGRAM: &str = "slipfork-world-init";
/// Fixed search path for helper subprocesses (`ip`, `mount`, `nsenter`); nothing is inherited.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
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
    status: WorldStatus,
    services: BTreeMap<String, ServiceRecord>,
}

#[derive(Debug)]
struct ServiceRecord {
    log: ServiceLogs,
    state: Arc<AtomicU8>,
    process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
struct ServiceProcess {
    supervisor: Option<Child>,
    inner_pid: Option<u32>,
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

    /// `nsenter` arguments entering every namespace of `init_pid` with `cwd` as working directory.
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
        let config = WorldInitConfig {
            netns_sock: netns_sock.clone(),
            mounts: spec
                .mounts
                .iter()
                .map(|mount| WorldInitMount {
                    host_path: mount.host_path.clone(),
                    guest_path: mount.guest_path.clone(),
                    read_only: mount.mode == MountMode::ReadOnly,
                })
                .collect(),
            local_ingress: spec.local_ingress.as_ref().map(|bridge| WorldInitIngress {
                port: bridge.port,
                host_ingress_sock: bridge.host_ingress_sock.clone(),
            }),
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
                && config.ready_path().exists()
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

    /// Stop every live service of `world` before its state is sealed or torn down.
    fn stop_live_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let running: Vec<String> = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            record
                .services
                .iter()
                .filter(|(_, service)| {
                    ServiceState::from_bits(service.state.load(Ordering::Relaxed))
                        == ServiceState::Running
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in running {
            self.stop_service_inner(world, &id)?;
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

        let (supervisor, inner_pid) = {
            let Ok(mut process) = process.lock() else {
                return Err(EngineError::Execution(format!(
                    "service '{id}' process state is poisoned"
                )));
            };
            (process.supervisor.take(), process.inner_pid)
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

        if let Some(pid) = inner_pid {
            let _ = linux_cow::signal_process(pid, ProcessSignal::Terminate);
        }
        if !wait_for_exit(&mut supervisor, SERVICE_STOP_GRACE) {
            if let Some(pid) = inner_pid {
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
        state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
        log.close();
        Ok(())
    }
}

impl WorldEngine for ReflinkNetnsEngine {
    fn name(&self) -> &'static str {
        "reflink-netns"
    }

    fn create_snapshot(
        &self,
        source_dir: &Path,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        if mode == SnapshotMode::StatefulRam {
            return Err(EngineError::StatefulRamUnsupported {
                engine: self.name(),
            });
        }
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

    fn freeze_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        if mode == SnapshotMode::StatefulRam {
            return Err(EngineError::StatefulRamUnsupported {
                engine: self.name(),
            });
        }
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
        let world = WorldHandle::mint(self.name(), seq);
        let label = format!("{}#{seq}", target_slug(&spec.target));
        let world_root = self.world_dir(&spec.target, seq);
        let state_dir = world_root.join("state");
        if let Err(err) = reflink_tree(&src, &state_dir) {
            let _ = fs::remove_dir_all(&world_root);
            return Err(err);
        }
        let booted = self.boot_world(&label, &world_root, spec);
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
        if spec.argv.is_empty() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!("service '{}' argv must not be empty", spec.id),
            });
        }

        let (init_pid, state_dir, env, log, state, process) = {
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
            let env = child_env::materialize(&record.base_env, &spec.env, |var| {
                std::env::var(var.name()).ok()
            });
            let log = ServiceLogs::new();
            let state = Arc::new(AtomicU8::new(ServiceState::Running.to_bits()));
            let process = Arc::new(Mutex::new(ServiceProcess::default()));
            record.services.insert(
                spec.id.clone(),
                ServiceRecord {
                    log: log.clone(),
                    state: Arc::clone(&state),
                    process: Arc::clone(&process),
                },
            );
            (init_pid, state_dir, env, log, state, process)
        };

        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, &spec.cwd))
            .args(&spec.argv)
            .env_clear()
            .envs(&env)
            .env("SLIPFORK_WORLD_STATE", &state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
                log.close();
                return Err(EngineError::Execution(format!(
                    "cannot start service '{}' ({}): {err}",
                    spec.id,
                    spec.argv.join(" ")
                )));
            }
        };

        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, StreamSource::Stdout, log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, StreamSource::Stderr, log.clone());
        }
        let supervisor_pid = child.id();
        if let Ok(mut process) = process.lock() {
            process.supervisor = Some(child);
        }
        // `nsenter --pid` forks: the service itself is the launcher's first child. `SIGTERM` must
        // reach that in-world pid, never the launcher parent.
        if let Some(inner_pid) = wait_for_inner_pid(supervisor_pid, SERVICE_STOP_GRACE)
            && let Ok(mut process) = process.lock()
        {
            process.inner_pid = Some(inner_pid);
        }
        spawn_service_watcher(Arc::clone(&process), Arc::clone(&state), log.clone());

        if let Some(port) = spec.readiness_port {
            let deadline = Instant::now() + SERVICE_READY_TIMEOUT;
            loop {
                match self.dial_world_tcp(world, port) {
                    Ok(_probe) => break,
                    Err(EngineError::DialFailed { .. }) => {}
                    Err(err) => {
                        abort_service(&process, &state, &log);
                        return Err(err);
                    }
                }
                if ServiceState::from_bits(state.load(Ordering::Relaxed)) != ServiceState::Running {
                    // The watcher already reaped the process and closed the log; the record stays
                    // `Exited` so a corrected retry with the same id is not blocked.
                    return Err(EngineError::Execution(format!(
                        "service '{}' exited before port {port} accepted connections",
                        spec.id
                    )));
                }
                if Instant::now() >= deadline {
                    // Never leave a half-started service running behind a failed spawn: kill and
                    // reap it, mark it `Stopped`, and keep its captured log for diagnosis so the
                    // caller can retry the same id.
                    abort_service(&process, &state, &log);
                    return Err(EngineError::Execution(format!(
                        "service '{}' did not accept connections on port {port} within {}s; \
                         the process was stopped (captured logs retained)",
                        spec.id,
                        SERVICE_READY_TIMEOUT.as_secs()
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
        let (init_pid, state_dir, env) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            (
                record.init_pid,
                record.state_dir.clone(),
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    std::env::var(var.name()).ok()
                }),
            )
        };

        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, cwd))
            .args(argv)
            .env_clear()
            .envs(&env)
            .env("SLIPFORK_WORLD_STATE", &state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| EngineError::Execution(format!("cannot enter world: {err}")))?;
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
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix: self.entry_prefix(init_pid, cwd),
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
fn abort_service(process: &Arc<Mutex<ServiceProcess>>, state: &Arc<AtomicU8>, log: &ServiceLogs) {
    let supervisor = process
        .lock()
        .ok()
        .and_then(|mut process| process.supervisor.take());
    if let Some(mut supervisor) = supervisor {
        if let Ok(children) = host_child_pids(supervisor.id())
            && let Some(inner_pid) = children.first()
        {
            let _ = linux_cow::signal_process(*inner_pid, ProcessSignal::Kill);
        }
        let _ = supervisor.kill();
        let _ = wait_for_exit(&mut supervisor, SERVICE_STOP_GRACE);
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
