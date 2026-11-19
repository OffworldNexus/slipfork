//! `PodBoot`/`ServicePod`: the daemon's per-service pod planning and spawn pipeline.
//!
//! One [`ServicePod`] is one `Forkfile` service materialized as its own `crun` container joined to
//! the world's shared `slipfork-world-init` user/network/IPC namespaces. The pod's bundle lives in
//! the engine control directory, its root filesystem is the immutable `L0` app tree, and its only
//! writable surface is the live worktree bind (`inotify` hot reload intact) plus each declared
//! `volume` CoW subvolume overlaid on top. Every value is injected; nothing here reads ambient
//! state, scans a manifest, or probes `/proc`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use slipfork_edge::{EdgeError, world_id};
use slipfork_forkfile::{EvaluatedFork, EvaluatedService, Span};
use slipfork_wire::control::BootEvent;
use slipfork_world::pod::{
    PodCommand, PodError, PodLocation, PodNamespaces, ServicePodSpec, VolumePlan,
};
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    ExecEnv, ServiceHandle, ServiceKind, ServiceRuntime, ServiceSpec, WorldEngine, WorldHandle,
};

use super::backing::service_specs;
use super::service::DaemonService;
use super::types::{LiveWorld, ResolvedComponent};
use super::{rejected, runtime_error};

/// One planned service pod: its id and the complete engine spec that spawns it.
pub(super) struct ServicePod {
    pub(super) spec: ServiceSpec,
    /// The `Forkfile` declaration this pod was planned from, quoted by spawn failures.
    pub(super) span: Span,
}

impl ServicePod {
    /// The pod's supervision id (the `Forkfile` service id).
    pub(super) fn id(&self) -> &str {
        &self.spec.id
    }
}

/// The injected per-world facts every service pod needs, resolved once after the world boots.
pub(super) struct PodBoot<'a> {
    engine: &'a dyn WorldEngine,
    handle: WorldHandle,
    worktree: PathBuf,
    world_subvol: PathBuf,
    namespaces: PodNamespaces,
    rootfs: WorldRootfs,
}

impl<'a> PodBoot<'a> {
    /// Resolve the world's subvolume and shared namespace joins before planning any pod.
    pub(super) fn new(
        engine: &'a dyn WorldEngine,
        handle: &WorldHandle,
        worktree: PathBuf,
        rootfs: WorldRootfs,
    ) -> Result<Self, EdgeError> {
        let world_subvol = engine.world_subvolume(handle).map_err(|err| {
            runtime_error(format!(
                "cannot resolve the world's CoW subvolume for its service pods: {err}\n  next: run `slipfork down && slipfork up` to re-clone the world"
            ))
        })?;
        let namespaces = engine.world_pod_namespaces(handle).map_err(|err| {
            runtime_error(format!(
                "cannot plan the world's service pods: {err}\n  next: run `slipfork up` to reboot the world and publish its namespaces"
            ))
        })?;
        Ok(Self {
            engine,
            handle: handle.clone(),
            worktree,
            world_subvol,
            namespaces,
            rootfs,
        })
    }

    /// Plan one declared `Forkfile` service as a pod.
    pub(super) fn component(
        &self,
        fork: &EvaluatedFork,
        component: &ResolvedComponent,
        base_env: &ExecEnv,
    ) -> Result<ServicePod, EdgeError> {
        let service = fork
            .services
            .iter()
            .find(|service| service.id == component.id)
            .ok_or_else(|| {
                rejected(format!(
                    "the world's Forkfile no longer declares service '{}'\n  next: run `slipfork restart` to re-evaluate it",
                    component.id
                ))
            })?;
        let directory = service_directory(service);
        if directory.as_os_str().is_empty() {
            return Err(rejected(format!(
                "service '{}' is pulled from `{}`; this boot path materializes Dockerfile services only\n  next: declare the service with a Dockerfile context (`service {} ./dir`)",
                component.id,
                pulled_reference(service),
                component.id
            )));
        }
        let cwd = self.worktree.join(&directory);
        let rootfs = self.rootfs.app_rootfs.clone();
        let bundle = self
            .engine
            .pod_bundle_dir(&self.handle, &component.id)
            .map_err(|err| {
                runtime_error(format!(
                    "cannot resolve the bundle directory of service pod '{}': {err}\n  next: run `slipfork doctor`; the pod bundle must exist in the world",
                    component.id
                ))
            })?;
        let volumes = VolumePlan::new(&self.world_subvol, &component.id, &cwd, &service.volumes)
            .map_err(pod_error)?;
        let command = PodCommand::new(
            cwd,
            component.argv.clone(),
            pod_environment(base_env, &component.env),
        );
        let location = PodLocation::new(&self.world_subvol, &bundle, &rootfs).map_err(pod_error)?;
        let pod = ServicePodSpec::new(location, self.namespaces.clone(), volumes, command)
            .map_err(pod_error)?;
        Ok(ServicePod {
            span: service.span,
            spec: ServiceSpec {
                kind: ServiceKind::Component,
                id: component.id.clone(),
                runtime: ServiceRuntime::ServicePod(pod),
                // The pod's own complete environment lives in its bundle command.
                env: ExecEnv::default(),
                readiness_port: component.readiness_port,
            },
        })
    }

    /// Spawn an already-planned pod and return its supervision handle.
    pub(super) fn spawn(&self, pod: &ServicePod) -> Result<ServiceHandle, EdgeError> {
        self.engine
            .spawn_service(&self.handle, &pod.spec)
            .map_err(|err| {
                runtime_error(format!(
                    "cannot start service pod '{}' (Forkfile:{}): {err}\n  next: run `slipfork doctor`; the pinned `crun` and the service bundle must exist in the world",
                    pod.id(),
                    pod.span.line
                ))
            })
    }
}

/// One component's complete container environment: the world base (which already carries the
/// inherited allowlist) overlaid with the service's own resolved layer, secrets last.
fn pod_environment(base: &ExecEnv, overlay: &ExecEnv) -> BTreeMap<String, String> {
    base.overlaid(overlay).expose_to_child_process()
}

/// The one-line remedy spelling of a pod planning refusal.
fn pod_error(err: PodError) -> EdgeError {
    rejected(err.to_string().replace("\n  fix:", "\n  next:"))
}

/// The declared service directory (`./api` -> `api`; a pulled image has none).
fn service_directory(service: &EvaluatedService) -> PathBuf {
    crate::project::service_directory(service)
}

/// The image reference of a pulled (non-Dockerfile) service, for the refusal.
fn pulled_reference(service: &EvaluatedService) -> String {
    match &service.image {
        slipfork_forkfile::EvaluatedImage::OciImage { image_ref } => image_ref.clone(),
        slipfork_forkfile::EvaluatedImage::Dockerfile { .. } => String::new(),
    }
}

impl DaemonService {
    /// Start one world's backing services, its component pods, and its deploy hooks.
    ///
    /// Every spec is planned **before** anything spawns, so a bundle/rootfs/volume refusal never
    /// leaves a half-started world. A spawn failure stops every service recorded on the world (the
    /// engine's own supervision record, `-pgid` groups included) before returning the one-line
    /// diagnostic.
    pub(super) fn start_world(
        &self,
        live: &mut LiveWorld,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        const SPAWN_PHASE: &str = "starting service pods";
        let world = world_id(&live.project, &live.target);
        let backing = service_specs(&live.fork, &live.rootfs)?;
        let boot = PodBoot::new(
            self.engine.as_ref(),
            &live.handle,
            live.worktree.clone(),
            live.rootfs.clone(),
        )?;
        let mut pods = Vec::new();
        for component in &live.components {
            // A service that declares no `run` command starts no process.
            if component.argv.is_empty() {
                continue;
            }
            pods.push(boot.component(&live.fork, component, &live.base_env)?);
        }

        let total = u8::try_from(backing.len() + pods.len()).unwrap_or(u8::MAX);
        let mut step = 0u8;
        let spawned = (|| -> Result<(), EdgeError> {
            for spec in backing {
                let id = spec.id.clone();
                step = step.saturating_add(1);
                on_event(BootEvent::Progress {
                    step,
                    total,
                    phase: SPAWN_PHASE.to_owned(),
                    detail: Some(id.clone()),
                });
                let service = boot.engine.spawn_service(&boot.handle, &spec).map_err(|err| {
                    runtime_error(format!(
                        "cannot start backing service '{id}' in world '{world}': {err}\n  next: run `slipfork doctor` and retry"
                    ))
                })?;
                live.services.push(service);
            }
            for pod in &pods {
                step = step.saturating_add(1);
                on_event(BootEvent::Progress {
                    step,
                    total,
                    phase: SPAWN_PHASE.to_owned(),
                    detail: Some(pod.id().to_owned()),
                });
                let service = boot.spawn(pod)?;
                live.services.push(service);
                if let Some(port) = pod.spec.readiness_port {
                    on_event(BootEvent::Ready {
                        component: pod.id().to_owned(),
                        port,
                    });
                }
            }
            Ok(())
        })();

        if let Err(error) = spawned {
            let teardown = self.engine.stop_world_services(&live.handle);
            live.services.clear();
            return Err(match teardown {
                Ok(()) => error,
                Err(err) => runtime_error(format!(
                    "{error}\n  next: run `slipfork down` to reap the world; its teardown also failed: {err}"
                )),
            });
        }

        let mut ran = self.run_hooks(live, slipfork_forkfile::HookPhase::PreDeploy, on_event)?;
        ran += self.run_hooks(live, slipfork_forkfile::HookPhase::PostDeploy, on_event)?;
        Ok(ran)
    }
}
