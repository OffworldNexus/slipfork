//! Streaming protocol verbs: exec targets, hooks, workspace refs, and forwards.
use std::io::Write;

use super::env::{forward_specs, hook_phase_label, open_hook_log, resolve_hook};
use super::protocol::services::{service_engine_error, service_not_running};
use super::service::DaemonService;
use super::{rejected, runtime_error};
use crate::project::{ExecEntry, select_exec_entry};
use slipfork_edge::{EdgeError, world_id};
use slipfork_forkfile::EvaluatedFork;
use slipfork_wire::control::{
    ExecTargetRequest, ForwardRequest, ForwardSummary, HookRunRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldState,
};
use slipfork_world::{ExecEnv, ServiceHandle, StreamChunk, WorldEngine, WorldOrigin};

/// The copied, lock-free world facts one entry target resolves against.
struct ExecWorld {
    id: String,
    worktree: std::path::PathBuf,
    fork: EvaluatedFork,
    services: Vec<ServiceHandle>,
    origin: WorldOrigin,
}

impl ExecWorld {
    /// The running pod of one declared app service; no app-rootfs fallback exists.
    fn service(&self, id: &str) -> Result<ServiceHandle, EdgeError> {
        self.services
            .iter()
            .find(|service| service.id() == id)
            .cloned()
            .ok_or_else(|| service_not_running(&self.id, id))
    }

    /// One declared app service, entered through its own `crun exec` container.
    fn app(
        &self,
        engine: &dyn WorldEngine,
        id: &str,
        argv: &[String],
    ) -> Result<WorldExecTarget, EdgeError> {
        self.fork
            .services
            .iter()
            .find(|service| service.id == id)
            .ok_or_else(|| service_not_running(&self.id, id))?;
        let service = self.service(id)?;
        let entry = engine
            .service_entry_command(&service)
            .map_err(|err| service_engine_error(&self.id, service.id(), err))?;
        Ok(WorldExecTarget {
            world: self.id.clone(),
            service: Some(id.to_owned()),
            origin: self.origin.clone(),
            entry,
            // The pod bundle's own environment is authoritative; never overlay the host or the
            // app rootfs PATH onto the container runtime.
            env: ExecEnv::default(),
            target_argv: argv.to_vec(),
        })
    }

    /// One declared backing resource: its running container and its own OCI environment.
    fn backing(
        &self,
        engine: &dyn WorldEngine,
        requested: &str,
        argv: &[String],
    ) -> Result<WorldExecTarget, EdgeError> {
        let (service_id, argv) =
            crate::project::resolve_backing_service(&self.fork, requested, argv)
                .map_err(rejected)?;
        let service = self
            .services
            .iter()
            .find(|service| service.id() == service_id.as_str())
            .cloned()
            .ok_or_else(|| service_not_running(&self.id, &service_id))?;
        let entry = engine
            .service_entry_command(&service)
            .map_err(|err| service_engine_error(&self.id, service.id(), err))?;
        Ok(WorldExecTarget {
            world: self.id.clone(),
            // The canonical container id, so `shell` renders `(slipfork:<world>/pg_clusters.main)`.
            service: Some(service_id),
            origin: self.origin.clone(),
            entry,
            // The container's own OCI environment is authoritative; never leak the app
            // rootfs PATH into `crun exec` (that would hide the image's `psql`/`redis-cli`).
            env: ExecEnv::default(),
            target_argv: argv,
        })
    }

    /// No declared service means no pod to enter: fail closed instead of an app-rootfs fallback.
    fn root(&self) -> Result<WorldExecTarget, EdgeError> {
        Err(rejected(format!(
            "world '{}' declares no app service; name one with `-s/--service <id>` or add a `service` block to the Forkfile",
            self.id
        )))
    }
}

impl DaemonService {
    pub(super) fn exec_target(
        &self,
        req: &ExecTargetRequest,
    ) -> Result<slipfork_wire::control::WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world = world_id(&selection.repo.project, &selection.target);
        let facts = {
            let state = self.lock();
            let Some(live) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                live.worktree.clone(),
                live.fork.clone(),
                live.services.clone(),
            )
        };
        let (worktree, fork, services) = facts;
        let world_facts = ExecWorld {
            id: world,
            worktree,
            fork,
            services,
            origin: self
                .registry
                .origin(&selection.repo.project, &selection.target)?,
        };
        match select_exec_entry(
            &world_facts.fork,
            &world_facts.worktree,
            &req.cwd,
            &req.service,
        )
        .map_err(rejected)?
        {
            ExecEntry::App(id) => world_facts.app(&*self.engine, &id, &req.argv),
            ExecEntry::Backing(id) => world_facts.backing(&*self.engine, &id, &req.argv),
            ExecEntry::World => world_facts.root(),
        }
    }
}

impl DaemonService {
    pub(super) fn hook_exec_target(
        &self,
        req: &HookRunRequest,
    ) -> Result<WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world = world_id(&selection.repo.project, &selection.target);
        let (hooks, services) = {
            let state = self.lock();
            let Some(live) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (live.hooks.clone(), live.services.clone())
        };
        let resolved = resolve_hook(&hooks, &req.hook)?;
        let service = services
            .iter()
            .find(|service| service.id() == resolved.service.as_str())
            .cloned()
            .ok_or_else(|| service_not_running(&world, &resolved.service))?;
        let entry = self
            .engine
            .service_entry_command(&service)
            .map_err(|err| service_engine_error(&world, service.id(), err))?;
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        Ok(WorldExecTarget {
            world,
            service: Some(resolved.service.clone()),
            origin,
            entry,
            // The pod bundle's own environment is authoritative for a hook in that container.
            env: ExecEnv::default(),
            target_argv: resolved.argv.clone(),
        })
    }
}

impl DaemonService {
    pub(super) fn workspace_refs(
        &self,
        req: &WorkspaceRefsRequest,
    ) -> Result<WorkspaceRefsSummary, EdgeError> {
        let selection = self.select(&req.cwd, Some(&req.fork))?;
        let world = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            if world.state != WorldState::Running && world.state != WorldState::Failed {
                return Err(rejected(format!(
                    "world '{}' is not live; run `slipfork up --world {}` first",
                    world_id(&selection.repo.project, &selection.target),
                    req.fork
                )));
            }
            world
                .workspace
                .clone()
                .map(|workspace| (workspace, world.worktree.clone()))
        };
        let (workspace, worktree) = world.ok_or_else(|| {
            rejected(format!(
                "fork '{}' has no materialized workspace in this slipforkd; create it with `slipfork fork {}`",
                req.fork, req.fork
            ))
        })?;
        let refs = workspace.refs();
        let base_oid = workspace
            .resolve(&refs.base)
            .map_err(|err| runtime_error(err.to_string()))?;
        let wip_oid = workspace
            .resolve(&refs.wip)
            .map_err(|err| runtime_error(err.to_string()))?;
        Ok(WorkspaceRefsSummary {
            base: refs.base,
            wip: refs.wip,
            base_oid,
            wip_oid,
            worktree,
        })
    }
}

impl DaemonService {
    pub(super) fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world = world_id(&selection.repo.project, &selection.target);
        let (hooks, services) = {
            let state = self.lock();
            let Some(live) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (live.hooks.clone(), live.services.clone())
        };
        let resolved = resolve_hook(&hooks, &req.hook)?;
        let service = services
            .iter()
            .find(|service| service.id() == resolved.service.as_str())
            .cloned()
            .ok_or_else(|| service_not_running(&world, &resolved.service))?;
        let phase = hook_phase_label(resolved.phase);
        let log = {
            let mut state = self.lock();
            let Some(world) = state.world_mut(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            self.hook_log(world, &req.hook, phase)?
        };
        let mut file = open_hook_log(&log.path)?;
        let output = self
            .engine
            .exec_in_service(&service, &resolved.argv, &resolved.overlay, &mut |chunk| {
                let _ = file.write_all(&chunk.bytes);
                let _ = file.flush();
                log.push(chunk.clone());
                on_chunk(chunk);
            })
            .map_err(|err| {
                log.close();
                service_engine_error(&world, service.id(), err)
            })?;
        log.close();
        Ok(output.exit_code)
    }
}

impl DaemonService {
    pub(super) fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world_id_text = world_id(&selection.repo.project, &selection.target);
        let (fork, components) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.fork.clone(), world.components.clone())
        };
        let specs = forward_specs(&fork, &world_id_text, &components, &req.targets)?;
        let bridge = self
            .registry
            .forward(&selection.repo.project, &selection.target, &specs)?;
        wait_disconnect(bridge.summary());
        // Dropping the bridge stops every accept loop and closes the host ports.
        Ok(())
    }
}

pub(crate) mod fan_in;
mod logs;
pub(crate) mod services;

#[cfg(test)]
mod tests;
