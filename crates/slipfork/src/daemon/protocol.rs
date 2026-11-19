//! Streaming protocol verbs: logs, exec targets, hooks, and forwards.
use std::io::Write;

use super::env::{forward_specs, open_hook_log, resolve_hook};
use super::service::DaemonService;
use super::types::RetainedTarget;
use super::{rejected, runtime_error};
use slipfork_edge::{EdgeError, world_id};
use slipfork_wire::control::{
    ExecTargetRequest, ForwardRequest, ForwardSummary, HookRunRequest, LogsRequest,
    WorkspaceRefsRequest, WorkspaceRefsSummary, WorldExecTarget, WorldState,
};
use slipfork_world::{ExecEnv, LogChunk, ServiceHandle, StreamChunk};
use std::sync::Arc;

impl DaemonService {
    pub(super) fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(LogChunk),
    ) -> Result<(), EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world_id_text = world_id(&selection.repo.project, &selection.target);
        let (handle, service_ids, hook_logs) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world
                    .services
                    .iter()
                    .map(ServiceHandle::id)
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                world.hook_logs.clone(),
            )
        };

        let mut retained: Vec<RetainedTarget> = Vec::new();
        let requested: Vec<String> = match &req.target {
            Some(target) => vec![target.clone()],
            None => service_ids
                .iter()
                .cloned()
                .chain(hook_logs.keys().cloned())
                .collect(),
        };
        for target in requested {
            if service_ids.iter().any(|id| id == &target) {
                retained.push(RetainedTarget::Service(target));
            } else if let Some(log) = hook_logs.get(&target) {
                retained.push(RetainedTarget::Hook(Arc::clone(log)));
            } else {
                let mut known = service_ids.clone();
                known.extend(hook_logs.keys().cloned());
                return Err(rejected(format!(
                    "world '{world_id_text}' has no log target '{target}' (known targets: {})",
                    known.join(", ")
                )));
            }
        }

        let wants = |chunk: &StreamChunk| {
            req.stderr == matches!(chunk.source, slipfork_world::StreamSource::Stderr)
        };
        for target in retained {
            match target {
                RetainedTarget::Service(id) => {
                    let log = self
                        .engine
                        .service_logs(&handle, &id)
                        .map_err(|err| runtime_error(err.to_string()))?;
                    // The engine's own record — never the request string — labels the chunks.
                    let label = log.target().to_owned();
                    let mut emit = |chunk: StreamChunk| {
                        if wants(&chunk) {
                            on_chunk(LogChunk::attribute(label.clone(), chunk));
                        }
                    };
                    if req.follow {
                        log.follow(&mut emit);
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            emit(chunk);
                        }
                    }
                }
                RetainedTarget::Hook(log) => {
                    let label = log.target().to_owned();
                    let mut emit = |chunk: StreamChunk| {
                        if wants(&chunk) {
                            on_chunk(LogChunk::attribute(label.clone(), chunk));
                        }
                    };
                    if req.follow {
                        log.follow(&mut emit);
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            emit(chunk);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl DaemonService {
    pub(super) fn exec_target(
        &self,
        req: &ExecTargetRequest,
    ) -> Result<slipfork_wire::control::WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world = world_id(&selection.repo.project, &selection.target);
        let (handle, base_env, service) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            let service = match &req.service {
                Some(requested) => {
                    let (service_id, argv) =
                        resolve_service_target(&world.snow, requested, &req.argv)?;
                    let handle = world
                        .services
                        .iter()
                        .find(|service| service.id() == service_id.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            service_not_running(
                                &world_id(&world.project, &world.target),
                                &service_id,
                            )
                        })?;
                    Some((handle, argv))
                }
                None => None,
            };
            (world.handle.clone(), world.base_env.clone(), service)
        };
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        if let Some((service, argv)) = service {
            let entry = self
                .engine
                .service_entry_command(&service)
                .map_err(|err| service_engine_error(&world, service.id(), err))?;
            return Ok(WorldExecTarget {
                world,
                origin,
                entry,
                // The container's own OCI environment is authoritative; never leak the app
                // rootfs PATH into `crun exec` (that would hide the image's `psql`/`redis-cli`).
                env: ExecEnv::default(),
                target_argv: argv,
            });
        }
        let entry = self
            .engine
            .entry_command_for(&handle, &req.cwd, req.entry_kind, &req.argv)
            .map_err(|err| runtime_error(err.to_string()))?;
        // One owner: the engine reports whether its prefix consumed the argv, so the request is
        // forwarded verbatim and `spawn_entry` decides from `entry.consumes_target_argv`.
        Ok(slipfork_wire::control::WorldExecTarget {
            world,
            origin,
            entry,
            env: base_env,
            target_argv: req.argv.clone(),
        })
    }
}

impl DaemonService {
    pub(super) fn hook_exec_target(
        &self,
        req: &HookRunRequest,
    ) -> Result<WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, worktree, snow, components, base_env) = {
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
                world.snow.clone(),
                world.components.clone(),
                world.base_env.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &components, &req.hook)?;
        let entry = self
            .engine
            .entry_command(&handle, &worktree)
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        Ok(WorldExecTarget {
            world: world_id(&selection.repo.project, &selection.target),
            origin,
            entry,
            env: base_env.overlaid(&resolved.overlay),
            target_argv: resolved.argv,
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
        let (handle, worktree, snow, components) = {
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
                world.snow.clone(),
                world.components.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &components, &req.hook)?;
        let log = {
            let mut state = self.lock();
            let Some(world) = state.world_mut(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            self.hook_log(world, &req.hook, resolved.phase_label)?
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
        let (snow, components) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.snow.clone(), world.components.clone())
        };
        let specs = forward_specs(&snow, &world_id_text, &components, &req.targets)?;
        let bridge = self
            .registry
            .forward(&selection.repo.project, &selection.target, &specs)?;
        wait_disconnect(bridge.summary());
        // Dropping the bridge stops every accept loop and closes the host ports.
        Ok(())
    }
}

pub(crate) mod services;

use services::{resolve_service_target, service_engine_error, service_not_running, tail_lines};
