//! Streaming protocol verbs: logs, exec targets, hooks, and forwards.
use std::io::Write;

use super::env::{forward_specs, open_hook_log, resolve_hook};
use super::service::DaemonService;
use super::types::RetainedTarget;
use super::{rejected, runtime_error};
use slipfork_edge::{EdgeError, world_id};
use slipfork_snow::SnowFile;
use slipfork_wire::control::{
    DaemonError, DaemonErrorKind, ExecTargetRequest, ForwardRequest, ForwardSummary,
    HookRunRequest, LogsRequest, WorkspaceRefsRequest, WorkspaceRefsSummary, WorldExecTarget,
    WorldState,
};
use slipfork_world::{EngineError, ExecEnv, LogChunk, ServiceHandle, StreamChunk};
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
            .entry_command(&handle, &req.cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
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

/// Resolve one `snow.yml` resource id to the running service handle id and the argv to run.
///
/// `pg_databases.main` selects the `pg_clusters.<cluster>` container and appends `-d main` to
/// `psql`; `redis_clusters.cache` is its own service. Nothing ever falls back to the app rootfs.
pub(super) fn resolve_service_target(
    snow: &SnowFile,
    requested: &str,
    argv: &[String],
) -> Result<(String, Vec<String>), EdgeError> {
    if requested.trim().is_empty() {
        return Err(rejected("a backing-service id must not be empty"));
    }
    let (kind, id) = requested.split_once('.').ok_or_else(|| {
        rejected(format!(
            "invalid backing-service id '{requested}' (expected <kind>.<id>, for example pg_databases.main)"
        ))
    })?;
    match kind {
        "pg_databases" => {
            let database = snow
                .resources
                .pg_databases
                .iter()
                .find(|database| database.id == id)
                .ok_or_else(|| unknown_service(snow, requested))?;
            let mut argv = argv.to_vec();
            if argv.first().map(String::as_str) == Some("psql")
                && !argv.iter().any(|arg| arg == "-d" || arg == "--dbname")
            {
                argv.push("-d".to_owned());
                argv.push(database.id.clone());
            }
            Ok((format!("pg_clusters.{}", database.cluster), argv))
        }
        "redis_clusters" => {
            if !snow
                .resources
                .redis_clusters
                .iter()
                .any(|cluster| cluster.id == id)
            {
                return Err(unknown_service(snow, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        "pg_clusters" => {
            if !snow
                .resources
                .pg_clusters
                .iter()
                .any(|cluster| cluster.id == id)
            {
                return Err(unknown_service(snow, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        _ => Err(unknown_service(snow, requested)),
    }
}

/// `ServiceNotRunning` diagnostic naming the world and the stopped container.
pub(super) fn service_not_running(world: &str, service: &str) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::ServiceNotRunning,
        format!(
            "world '{world}' has no running service '{service}' (ServiceNotRunning): psql and \
             redis-cli run only inside the service container — run `slipfork up` and retry"
        ),
    ))
}

/// `ServiceNotRunning` remains a typed daemon error; every other engine failure is internal.
pub(super) fn service_engine_error(world: &str, service: &str, error: EngineError) -> EdgeError {
    match error {
        EngineError::ServiceNotRunning { .. } => service_not_running(world, service),
        other => runtime_error(other.to_string()),
    }
}

pub(super) fn unknown_service(snow: &SnowFile, requested: &str) -> EdgeError {
    let mut known: Vec<String> = snow
        .resources
        .pg_databases
        .iter()
        .map(|database| format!("pg_databases.{}", database.id))
        .chain(
            snow.resources
                .redis_clusters
                .iter()
                .map(|cluster| format!("redis_clusters.{}", cluster.id)),
        )
        .collect();
    known.sort();
    rejected(format!(
        "world declares no backing service '{requested}' (known services: {})",
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    ))
}

/// Keep the last `lines` lines of the retained stdout/stderr selection.
pub(super) fn tail_lines(chunks: Vec<StreamChunk>, lines: Option<usize>) -> Vec<StreamChunk> {
    let Some(lines) = lines else {
        return chunks;
    };
    let joined: Vec<u8> = chunks
        .iter()
        .flat_map(|chunk| chunk.bytes.iter().copied())
        .collect();
    let text = String::from_utf8_lossy(&joined);
    let kept: Vec<&str> = text
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut bytes = kept.join("\n").into_bytes();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    let source = chunks
        .first()
        .map_or(slipfork_world::StreamSource::Stdout, |chunk| chunk.source);
    vec![StreamChunk { source, bytes }]
}
