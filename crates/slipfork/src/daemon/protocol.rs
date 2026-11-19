//! Streaming protocol verbs: logs, exec targets, hooks, and forwards.
use std::io::Write;

use super::env::{forward_specs, open_hook_log, resolve_hook};
use super::service::DaemonService;
use super::{rejected, runtime_error};
use slipfork_edge::{EdgeError, world_id};
use slipfork_wire::control::{
    ExecTargetRequest, ForwardRequest, ForwardSummary, HookRunRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldState,
};
use slipfork_world::{ExecEnv, StreamChunk};

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

use services::{resolve_service_target, service_engine_error, service_not_running};

pub(crate) mod fan_in;
mod logs;
pub(crate) mod services;

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use slipfork_world::{LogStream, ServiceLogs, StreamChunk, StreamSource, epoch_millis_now};

    use super::super::types::{HookLog, LogBus};
    use super::fan_in::{self, FanIn, LogSource};

    /// Wait for the next follower chunk, failing the test rather than hanging forever.
    fn next(receiver: &mpsc::Receiver<slipfork_world::LogChunk>) -> slipfork_world::LogChunk {
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the follower must stay attached")
    }

    #[test]
    fn divider_appears_on_restart_and_down_up_while_a_follower_stays_attached() {
        let bus = Arc::new(LogBus::default());
        bus.boot(epoch_millis_now());
        let epochs: Arc<Mutex<Vec<ServiceLogs>>> =
            Arc::new(Mutex::new(vec![ServiceLogs::new("api")]));

        let (sender, receiver) = mpsc::channel();
        let follower_bus = Arc::clone(&bus);
        let follower_epochs = Arc::clone(&epochs);
        let aborted = Arc::new(AtomicBool::new(false));
        let follower = thread::spawn(move || {
            let _ = fan_in::follow_bus(
                &follower_bus,
                || {
                    Ok(follower_epochs
                        .lock()
                        .expect("epochs")
                        .last()
                        .map(|log| vec![LogSource::Service(log.clone())])
                        .unwrap_or_default())
                },
                false,
                None,
                &aborted,
                &mut |chunk| {
                    let _ = sender.send(chunk);
                    ControlFlow::Continue(())
                },
            );
        });

        // Epoch 0: the first boot streams its output.
        epochs.lock().expect("epochs")[0].push(StreamSource::Stdout, b"before\n".to_vec());
        assert_eq!(next(&receiver).bytes, b"before\n");

        // `restart`: the old window closes and the daemon publishes one epoch.
        epochs.lock().expect("epochs")[0].close();
        let second = ServiceLogs::new("api");
        second.push(StreamSource::Stdout, b"after\n".to_vec());
        epochs.lock().expect("epochs").push(second.clone());
        bus.boot(epoch_millis_now());

        let divider = next(&receiver);
        assert!(
            divider.is_event(),
            "restart must emit a divider: {divider:?}"
        );
        assert_eq!(next(&receiver).bytes, b"after\n");

        // `down`: the world record and its windows disappear; the bus and follower do not.
        second.close();
        epochs.lock().expect("epochs").clear();
        let third = ServiceLogs::new("api");
        third.push(StreamSource::Stdout, b"after-up\n".to_vec());
        epochs.lock().expect("epochs").push(third.clone());
        bus.boot(epoch_millis_now());

        assert!(
            next(&receiver).is_event(),
            "down + up must emit its own divider"
        );
        assert_eq!(next(&receiver).bytes, b"after-up\n");

        third.close();
        bus.shutdown();
        let started = Instant::now();
        follower.join().expect("the follower thread reaped");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_reused_hook_window_emits_only_new_lines_across_two_epochs() {
        let bus = Arc::new(LogBus::default());
        bus.boot(epoch_millis_now());
        let hook = Arc::new(HookLog::new(
            "api:pre-deploy".to_owned(),
            PathBuf::from("/var/tmp/slipfork-replay.log"),
        ));
        hook.push(StreamChunk {
            source: StreamSource::Stdout,
            bytes: b"before\n".to_vec(),
        });

        let (sender, receiver) = mpsc::channel();
        let follower_bus = Arc::clone(&bus);
        let follower_hook = Arc::clone(&hook);
        let aborted = Arc::new(AtomicBool::new(false));
        let follower = thread::spawn(move || {
            let _ = fan_in::follow_bus(
                &follower_bus,
                || Ok(vec![LogSource::Hook(Arc::clone(&follower_hook))]),
                false,
                None,
                &aborted,
                &mut |chunk| {
                    let _ = sender.send(chunk);
                    ControlFlow::Continue(())
                },
            );
        });

        assert_eq!(next(&receiver).bytes, b"before\n");

        // Epoch 2 reuses the same window: only the newly appended line may be emitted.
        hook.push(StreamChunk {
            source: StreamSource::Stdout,
            bytes: b"after\n".to_vec(),
        });
        bus.boot(epoch_millis_now());
        hook.close();
        bus.shutdown();
        follower.join().expect("the follower thread reaped");

        // The first `next` above consumed the one legitimate history line; nothing after it may
        // repeat that history.
        let chunks: Vec<_> = receiver.into_iter().collect();
        assert!(
            chunks.iter().all(|chunk| chunk.bytes != b"before\n"),
            "history must never be re-emitted: {chunks:?}"
        );
        let afters = chunks
            .iter()
            .filter(|chunk| chunk.bytes == b"after\n")
            .count();
        assert_eq!(
            afters, 1,
            "new lines must be emitted exactly once: {chunks:?}"
        );
    }

    #[test]
    fn dropping_the_consumer_aborts_the_reader_threads_promptly() {
        let bus = Arc::new(LogBus::default());
        bus.boot(epoch_millis_now());
        let log = ServiceLogs::new("api");
        log.push(StreamSource::Stdout, b"first\n".to_vec());
        let aborted = AtomicBool::new(false);
        let started = Instant::now();
        let mut emitted = 0;

        fan_in::follow_bus(
            &bus,
            || Ok(vec![LogSource::Service(log.clone())]),
            false,
            None,
            &aborted,
            &mut |_chunk| {
                emitted += 1;
                ControlFlow::Break(())
            },
        )
        .expect("an aborted follower is not an error");

        assert_eq!(emitted, 1, "the client saw exactly the chunk it broke on");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the source reader must be reaped, not left blocked on the log condvar"
        );
    }

    #[test]
    fn tail_preserves_each_lines_stream_and_capture_time() {
        let log = ServiceLogs::new("api");
        log.push_at(10, StreamSource::Stdout, b"out1\nout2\n".to_vec());
        log.push_at(20, StreamSource::Stderr, b"err1\n".to_vec());
        let mut fan = FanIn::new(vec![LogSource::Service(log)], false);
        let tailed = fan.tail(2);

        assert_eq!(tailed.len(), 2, "two lines are kept: {tailed:?}");
        assert_eq!(tailed[0].bytes, b"out2\n");
        assert_eq!(tailed[0].stream, LogStream::Stdout);
        assert_eq!(tailed[0].ts_ms, 10);
        assert_eq!(tailed[1].bytes, b"err1\n");
        assert_eq!(tailed[1].stream, LogStream::Stderr);
        assert_eq!(tailed[1].ts_ms, 20);
    }

    #[test]
    fn collect_merges_stdout_and_stderr_by_capture_time() {
        let log = ServiceLogs::new("api");
        log.push_at(30, StreamSource::Stderr, b"third\n".to_vec());
        log.push_at(10, StreamSource::Stdout, b"first\n".to_vec());
        log.push_at(20, StreamSource::Stdout, b"second\n".to_vec());
        let merged = FanIn::new(vec![LogSource::Service(log)], false).merged_snapshot();
        let text: String = merged
            .iter()
            .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
            .collect();
        assert_eq!(text, "first\nsecond\nthird\n");
        assert!(merged.iter().all(|chunk| !chunk.is_event()));
    }
}
