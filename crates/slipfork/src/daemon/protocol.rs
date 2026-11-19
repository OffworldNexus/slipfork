//! Streaming protocol verbs: exec targets, hooks, workspace refs, and forwards.
use std::io::Write;

use super::env::{forward_specs, hook_phase_label, open_hook_log, resolve_hook};
use super::protocol::services::{service_engine_error, service_not_running};
use super::service::DaemonService;
use super::types::ResolvedComponent;
use super::{rejected, runtime_error};
use crate::project::{ExecEntry, select_exec_entry, service_directory};
use slipfork_edge::{EdgeError, world_id};
use slipfork_forkfile::EvaluatedFork;
use slipfork_wire::control::{
    ExecTargetRequest, ForwardRequest, ForwardSummary, HookRunRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldState,
};
use slipfork_world::{
    EntryKind, ExecEnv, ServiceHandle, StreamChunk, WorldEngine, WorldHandle, WorldOrigin,
};

/// The copied, lock-free world facts one entry target resolves against.
struct ExecWorld {
    id: String,
    handle: WorldHandle,
    base_env: ExecEnv,
    worktree: std::path::PathBuf,
    fork: EvaluatedFork,
    components: Vec<ResolvedComponent>,
    services: Vec<ServiceHandle>,
    origin: WorldOrigin,
}

impl ExecWorld {
    /// One declared app service: its own directory and the component's live environment overlay.
    fn app(
        &self,
        engine: &dyn WorldEngine,
        id: &str,
        kind: EntryKind,
        argv: &[String],
    ) -> Result<WorldExecTarget, EdgeError> {
        let component = self
            .components
            .iter()
            .find(|component| component.id == id)
            .ok_or_else(|| service_not_running(&self.id, id))?;
        let service = self
            .fork
            .services
            .iter()
            .find(|service| service.id == id)
            .ok_or_else(|| service_not_running(&self.id, id))?;
        let cwd = self.worktree.join(service_directory(service));
        let entry = engine
            .entry_command_for(&self.handle, &cwd, kind, argv)
            .map_err(|err| runtime_error(err.to_string()))?;
        Ok(WorldExecTarget {
            world: self.id.clone(),
            service: Some(id.to_owned()),
            origin: self.origin.clone(),
            entry,
            env: self.base_env.overlaid(&component.env),
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

    /// The world's app stage itself: no declared service for this `cwd`.
    fn root(
        &self,
        engine: &dyn WorldEngine,
        cwd: &std::path::Path,
        kind: EntryKind,
        argv: &[String],
    ) -> Result<WorldExecTarget, EdgeError> {
        let entry = engine
            .entry_command_for(&self.handle, cwd, kind, argv)
            .map_err(|err| runtime_error(err.to_string()))?;
        // One owner: the engine reports whether its prefix consumed the argv, so the request is
        // forwarded verbatim and `spawn_entry` decides from `entry.consumes_target_argv`.
        Ok(WorldExecTarget {
            world: self.id.clone(),
            service: None,
            origin: self.origin.clone(),
            entry,
            env: self.base_env.clone(),
            target_argv: argv.to_vec(),
        })
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
                live.handle.clone(),
                live.base_env.clone(),
                live.worktree.clone(),
                live.fork.clone(),
                live.components.clone(),
                live.services.clone(),
            )
        };
        let (handle, base_env, worktree, fork, components, services) = facts;
        let world_facts = ExecWorld {
            id: world,
            handle,
            base_env,
            worktree,
            fork,
            components,
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
            ExecEntry::App(id) => world_facts.app(&*self.engine, &id, req.entry_kind, &req.argv),
            ExecEntry::Backing(id) => world_facts.backing(&*self.engine, &id, &req.argv),
            ExecEntry::World => {
                world_facts.root(&*self.engine, &req.cwd, req.entry_kind, &req.argv)
            }
        }
    }
}

impl DaemonService {
    pub(super) fn hook_exec_target(
        &self,
        req: &HookRunRequest,
    ) -> Result<WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, worktree, hooks, base_env) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world.worktree.clone(),
                world.hooks.clone(),
                world.base_env.clone(),
            )
        };
        let resolved = resolve_hook(&hooks, &req.hook)?;
        let entry = self
            .engine
            .entry_command(&handle, &worktree)
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        Ok(WorldExecTarget {
            world: world_id(&selection.repo.project, &selection.target),
            service: Some(resolved.service.clone()),
            origin,
            entry,
            env: base_env.overlaid(&resolved.overlay),
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
        let (handle, worktree, hooks) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world.worktree.clone(),
                world.hooks.clone(),
            )
        };
        let resolved = resolve_hook(&hooks, &req.hook)?;
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
            .exec_in_world(
                &handle,
                &resolved.argv,
                &worktree,
                &resolved.overlay,
                &mut |chunk| {
                    let _ = file.write_all(&chunk.bytes);
                    let _ = file.flush();
                    log.push(chunk.clone());
                    on_chunk(chunk);
                },
            )
            .map_err(|err| {
                log.close();
                runtime_error(err.to_string())
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
