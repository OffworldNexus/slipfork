//! Service supervision rows and the `WorldEngine` service verbs: spawn, stop, query, and re-apply
//! an environment while keeping the retained log (`ADR-0002` §2, `ADR-0009` §3).
//!
//! Every service — a `Forkfile` service pod, a legacy world process, or a backing service — is a
//! daemonless `crun` container; the legacy variants differ only in which bundle is synthesized.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::rootfs::{BackingServiceKey, WorldRootfs};
use slipfork_world::{
    EngineError, ExecEnv, InheritedVar, ServiceHandle, ServiceKind, ServiceLogs, ServicePodSpec,
    ServiceRuntime, ServiceSpec, ServiceState, StateSubdirMount, WorldHandle, WorldMount,
    WorldServiceInfo,
};

use crate::child_env;

use super::BtrfsEngine;
use super::container::ContainerProcess;
use super::teardown::{SERVICE_STOP_GRACE, ServiceTeardown};

/// Bound on a container accepting connections (images can be slow on first boot).
const OCI_SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct ServiceRecord {
    pub(crate) spec: ServiceSpec,
    pub(crate) kind: ServiceKind,
    pub(crate) port: Option<u16>,
    pub(crate) started_at: Instant,
    pub(crate) log: ServiceLogs,
    pub(crate) state: Arc<AtomicU8>,
    pub(crate) process: Arc<Mutex<ServiceProcess>>,
}

#[derive(Debug, Default)]
pub(crate) struct ServiceProcess {
    pub(crate) supervisor: Option<Child>,
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

/// The engine's own spawn paths, decided once from a [`ServiceSpec`].
enum SpawnPlan<'a> {
    /// A test-only `WorldProcess` path (scheduled for deletion in `M3.32.2.2.1.3.2.2`), run as a
    /// pod-shaped container on the app rootfs.
    World { argv: &'a [String], cwd: &'a Path },
    /// A backing-service container.
    Container {
        image: &'a BackingServiceKey,
        argv: &'a [String],
        state_mounts: &'a [StateSubdirMount],
        readonly_mounts: &'a [WorldMount],
    },
    /// A `Forkfile` service pod.
    Pod(&'a ServicePodSpec),
}

impl SpawnPlan<'_> {
    fn argv(&self) -> &[String] {
        match self {
            Self::World { argv, .. } | Self::Container { argv, .. } => argv,
            Self::Pod(pod) => pod.argv(),
        }
    }
}

impl BtrfsEngine {
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
        let plan = match &spec.runtime {
            // Test-only path, scheduled for deletion in `M3.32.2.2.1.3.2.2`.
            ServiceRuntime::WorldProcess { argv, cwd } => SpawnPlan::World { argv, cwd },
            ServiceRuntime::OciContainer {
                image,
                argv,
                state_mounts,
                readonly_mounts,
            } => SpawnPlan::Container {
                image,
                argv,
                state_mounts,
                readonly_mounts,
            },
            ServiceRuntime::ServicePod(pod) => SpawnPlan::Pod(pod),
        };
        if plan.argv().is_empty() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!("service '{}' argv must not be empty", spec.id),
            });
        }
        // A pod's complete environment lives in its `PodCommand`. Retain it on the service record
        // so `crun exec` re-applies exactly the environment the container was created with.
        let retained = {
            let mut retained = spec.clone();
            if let ServiceRuntime::ServicePod(pod) = &spec.runtime {
                retained.env = pod_exec_env(pod)?;
            }
            retained
        };
        let spec = &retained;

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
            let env = match &spec.runtime {
                // Test-only path, scheduled for deletion in `M3.32.2.2.1.3.2.2`.
                ServiceRuntime::WorldProcess { .. } => {
                    child_env::materialize(&record.base_env, &spec.env, |var| {
                        self.inherited_env.get(var)
                    })
                }
                ServiceRuntime::OciContainer { .. } | ServiceRuntime::ServicePod(_) => {
                    child_env::materialize_container(&spec.env, |var| self.inherited_env.get(var))
                }
            };
            let log = ServiceLogs::new(spec.id.as_str());
            let state = Arc::new(AtomicU8::new(ServiceState::Running.to_bits()));
            let process = Arc::new(Mutex::new(ServiceProcess::default()));
            record.services.insert(
                spec.id.clone(),
                ServiceRecord {
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
        let spawned = match plan {
            SpawnPlan::World { argv, cwd } => self.spawn_world_process_pod(&spawn, argv, cwd),
            SpawnPlan::Container {
                image,
                argv,
                state_mounts,
                readonly_mounts,
            } => self.spawn_oci_container(&spawn, image, argv, state_mounts, readonly_mounts),
            SpawnPlan::Pod(pod) => self.spawn_service_pod(&spawn, pod),
        };
        if let Err(err) = spawned {
            state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
            log.close();
            return Err(err);
        }
        spawn_service_watcher(Arc::clone(&process), Arc::clone(&state), log.clone());

        if let Some(port) = spec.readiness_port {
            let ready = crate::wait::poll_until(OCI_SERVICE_READY_TIMEOUT, || {
                match self.open_world_tcp(world, port) {
                    Ok(_probe) => return Some(Ok(())),
                    Err(EngineError::DialFailed { .. }) => {}
                    Err(err) => return Some(Err(err)),
                }
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
                    let _ =
                        self.stop_service_process(&spec.id, &state, &log, &process, Duration::ZERO);
                    return Err(EngineError::Execution(format!(
                        "service '{}' did not accept connections on port {port} within {}s; \
                         the container was stopped (captured logs retained)",
                        spec.id,
                        OCI_SERVICE_READY_TIMEOUT.as_secs()
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
        let mut spec = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, service.world())?;
            record
                .services
                .get(service.id())
                .ok_or_else(|| EngineError::UnknownService {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                })?
                .spec
                .clone()
        };
        // A world process is a pod-shaped container whose bundle is rewritten on every spawn:
        // stopping the old container lets the new environment be the one it is created with.
        // A service pod's bundle is also rewritten: the overlay updates the pod's complete
        // container environment while its base and argv stay as planned. A backing-service OCI
        // bundle is fixed at spawn.
        match &spec.runtime {
            // Test-only path, scheduled for deletion in `M3.32.2.2.1.3.2.2`.
            ServiceRuntime::WorldProcess { .. } => {
                spec.env = env.clone();
                self.stop_service_inner(service.world(), service.id())?;
            }
            ServiceRuntime::ServicePod(pod) => {
                let mut pod = pod.clone();
                for (name, value) in env.expose_to_child_process() {
                    pod.command.env.insert(name, value);
                }
                spec.runtime = ServiceRuntime::ServicePod(pod);
                self.stop_service_inner(service.world(), service.id())?;
            }
            ServiceRuntime::OciContainer { .. } => {
                return Err(EngineError::ContainerEnvFixed {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                });
            }
        }
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
            record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect()
        };
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
            let pid = if running {
                let process = service.process.lock().map_err(|_| {
                    EngineError::Execution(format!(
                        "service '{id}' supervision record in world {world:?} is poisoned"
                    ))
                })?;
                process.supervisor.as_ref().map(Child::id)
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

/// The exec environment of one pod: exactly the container's declared environment, validated.
fn pod_exec_env(pod: &ServicePodSpec) -> Result<ExecEnv, EngineError> {
    let mut env = ExecEnv::default();
    for (name, value) in &pod.command.env {
        // The pod's environment is verbatim, but `PATH` (and the other inherit-allowed vars) are
        // reserved names: they are recorded as inherited values, never re-bound as plain ones.
        if let Some(var) = InheritedVar::parse(name) {
            env.inherit(var, value.clone());
            continue;
        }
        env.set(name, value.clone())
            .map_err(|err| EngineError::InvalidEnvName {
                name: err.name,
                reason: err.reason,
            })?;
    }
    Ok(env)
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
