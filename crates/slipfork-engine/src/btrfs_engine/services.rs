//! Service supervision rows and the `WorldEngine` service verbs: spawn, stop, query, and re-apply
//! an environment while keeping the retained log (`ADR-0002` §2, `ADR-0009` §3).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, ExecEnv, ServiceHandle, ServiceKind, ServiceLogs, ServiceRuntime, ServiceSpec,
    ServiceState, StreamSource, WorldHandle, WorldMount, WorldServiceInfo,
};

use crate::child_env;
use crate::child_io::spawn_log_reader;
use crate::supervise::{ProcessIdentity, kill_and_reap_child, wait_for_inner_pid};
use crate::wait::poll_until;
use crate::world_init::WORLD_STATE_GUEST;

use super::BtrfsEngine;
use super::container::ContainerProcess;
use super::teardown::{SERVICE_STOP_GRACE, ServiceTeardown};

/// Bound on a backing-service container accepting connections (images can be slow on first boot).
const OCI_SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on waiting for `ServiceSpec::readiness_port` to accept connections.
const SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Which spawn path produced a supervised service — the engine's private teardown/readiness
/// discriminator. The wire-facing classification is [`ServiceKind`] (component vs backing service).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeKind {
    WorldProcess,
    OciContainer,
}

#[derive(Debug)]
pub(crate) struct ServiceRecord {
    /// Which spawn path produced it (teardown/readiness discriminator).
    pub(crate) runtime_kind: RuntimeKind,
    /// The complete spawn contract retained for [`WorldEngine::update_service_env`]: argv, cwd,
    /// mounts, and readiness are the engine's to own so the daemon never re-authors a
    /// `ServiceSpec` to apply a new environment.
    pub(crate) spec: ServiceSpec,
    /// The caller-declared row classification (`ServiceSpec::kind`).
    pub(crate) kind: ServiceKind,
    /// In-world serving/readiness port the caller asked for, recorded at spawn.
    pub(crate) port: Option<u16>,
    /// Spawn instant of the service; `uptime_secs` is measured from it.
    pub(crate) started_at: Instant,
    pub(crate) log: ServiceLogs,
    pub(crate) state: Arc<AtomicU8>,
    pub(crate) process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
pub(crate) struct ServiceProcess {
    pub(crate) supervisor: Option<Child>,
    pub(crate) inner: Option<ProcessIdentity>,
    /// The group this service's launcher leads; stop signals the whole group so a descendant that
    /// outlived the supervised child is still reaped.
    pub(crate) process_group: Option<ProcessIdentity>,
    pub(crate) container: Option<ContainerProcess>,
}

/// Everything a spawn path needs from the already-locked world record.
pub(crate) struct ServiceSpawn<'a> {
    pub(crate) world: &'a WorldHandle,
    pub(crate) init_pid: u32,
    pub(crate) ctl_dir: &'a Path,
    pub(crate) state_dir: &'a Path,
    pub(crate) rootfs: &'a WorldRootfs,
    pub(crate) mounts: &'a [WorldMount],
    pub(crate) env: &'a BTreeMap<String, String>,
    pub(crate) spec: &'a ServiceSpec,
    pub(crate) log: &'a ServiceLogs,
    pub(crate) process: &'a Arc<Mutex<ServiceProcess>>,
}

impl BtrfsEngine {
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
            spawn_log_reader(stdout, StreamSource::Stdout, spawn.log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, StreamSource::Stderr, spawn.log.clone());
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

    pub(crate) fn start_service(
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
            // Invariant: an OCI container bundle must never carry the world component environment.
            // `record.base_env` holds every resolved `modes.*.envs.*` value (secrets included) and
            // is persisted verbatim into the bundle's `config.json`; a container therefore gets
            // only the inherited allowlist plus its own `spec.env`. World processes (components
            // and hooks) legitimately need the world env.
            let env = match runtime_kind {
                RuntimeKind::WorldProcess => {
                    child_env::materialize(&record.base_env, &spec.env, |var| {
                        self.inherited_env.get(var)
                    })
                }
                RuntimeKind::OciContainer => {
                    child_env::materialize_container(&spec.env, |var| self.inherited_env.get(var))
                }
            };
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
            let ready = poll_until(timeout, || {
                match self.open_world_tcp(world, port) {
                    Ok(_probe) => return Some(Ok(())),
                    Err(EngineError::DialFailed { .. }) => {}
                    Err(err) => return Some(Err(err)),
                }
                // The watcher reaps a self-exited process and closes the log; the record stays
                // `Exited` so a corrected retry with the same id is not blocked.
                (ServiceState::from_bits(state.load(Ordering::Relaxed)) != ServiceState::Running)
                    .then_some(Err(EngineError::Execution(format!(
                        "service '{}' exited before port {port} accepted connections (captured \
                         logs retained)",
                        spec.id
                    ))))
            });
            match ready {
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    if ServiceState::from_bits(state.load(Ordering::Relaxed))
                        == ServiceState::Running
                    {
                        let _ = self.stop_service_process(
                            &spec.id,
                            &state,
                            &log,
                            &process,
                            Duration::ZERO,
                        );
                    }
                    return Err(err);
                }
                None => {
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
            }
        }

        Ok(ServiceHandle::mint(world.clone(), spec.id.clone(), state))
    }

    pub(crate) fn restart_with_env(
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
        // A container's bundle `config.json` is composed once, from its own declared env, at
        // spawn. Respawning it with a caller's world env would persist that env (secrets included)
        // onto disk, so the update is refused before the running container is touched.
        if matches!(spec.runtime, ServiceRuntime::OciContainer { .. }) {
            return Err(EngineError::ContainerEnvFixed {
                world: format!("{:?}", service.world()),
                id: service.id().to_owned(),
            });
        }
        // The one stop path first, so a failed respawn never leaves two processes under one id.
        self.stop_service_inner(service.world(), service.id())?;
        // A world process re-derives `record.base_env ⊕ spec.env` plus the inherited allowlist at
        // spawn, so an env update can neither drop nor smuggle the world environment.
        spec.env = env.clone();
        self.start_service(service.world(), &spec)?;
        Ok(())
    }

    pub(crate) fn stop_service_handle(&self, service: &ServiceHandle) -> Result<(), EngineError> {
        self.stop_service_inner(service.world(), service.id())
    }

    pub(crate) fn stop_all_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
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

    pub(crate) fn retained_logs(
        &self,
        world: &WorldHandle,
        id: &str,
    ) -> Result<ServiceLogs, EngineError> {
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

    pub(crate) fn service_rows(
        &self,
        world: &WorldHandle,
    ) -> Result<Vec<WorldServiceInfo>, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        let now = Instant::now();
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
