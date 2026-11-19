//! The `logs` verb: one chronological stream over every retained service and hook window.
//!
//! A non-follow request snapshots every source, filters the requested stream, tails, and merges by
//! capture `ts_ms`. Follow keeps one reader thread per source alive across restart epochs through
//! [`fan_in::follow_bus`]; the world's [`super::super::types::LogBus`] wakes it with a divider
//! instead of ending the stream.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use slipfork_edge::{EdgeError, world_id};
use slipfork_wire::control::LogsRequest;
use slipfork_world::{LogChunk, ServiceHandle};

use super::super::rejected;
use super::super::service::DaemonService;
use super::fan_in;
use super::services::service_engine_error;

impl DaemonService {
    /// Read or follow one world's retained logs as one chronological stream.
    ///
    /// Non-follow: every source is snapshotted, filtered to the requested stream, tailed, and merged
    /// by capture `ts_ms`. Follow: one reader thread per service **and** per hook feeds one channel,
    /// merged by capture time (stable tiebreak: target, then arrival), while the restart bus wakes
    /// the follower with a divider on `restart` and `down` + `up` instead of ending the stream.
    pub(crate) fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> Result<(), EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let project = selection.repo.project.clone();
        let target = selection.target.clone();
        let world_id_text = world_id(&project, &target);

        // Validate the request against the world held *now*: an unknown target fails closed before
        // any follow loop starts. The world identity, not the record, is what a follower tracks.
        {
            let (service_ids, hook_ids) = {
                let state = self.lock();
                let Some(world) = state.world(&project, &target) else {
                    return Err(Self::unknown_world(&project, &target));
                };
                (
                    world
                        .services
                        .iter()
                        .map(ServiceHandle::id)
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                    world.hook_logs.keys().cloned().collect::<Vec<_>>(),
                )
            };
            if let Some(requested) = &req.target
                && !service_ids.iter().any(|id| id == requested)
                && !hook_ids.iter().any(|id| id == requested)
            {
                let mut known = service_ids.clone();
                known.extend(hook_ids.iter().cloned());
                return Err(rejected(format!(
                    "world '{world_id_text}' has no log target '{requested}' (known targets: {}); next: run `slipfork logs` to follow every target",
                    known.join(", ")
                )));
            }
        }

        if !req.follow {
            let sources = self.log_sources(&project, &target, req.target.as_deref())?;
            let mut fan = fan_in::FanIn::new(sources, req.stderr);
            let chunks = match req.lines {
                Some(lines) => fan.tail(lines),
                None => fan.merged_snapshot(),
            };
            for chunk in chunks {
                if on_chunk(chunk).is_break() {
                    break;
                }
            }
            return Ok(());
        }

        // A fan-out follows the world's *current* target set on every re-attach, so a service added
        // by an edited `snow.yml` after `restart` is picked up without reconnecting.
        let requested = req.target.clone();
        let bus = self.log_bus(&project, &target);
        let aborted = AtomicBool::new(false);
        fan_in::follow_bus(
            &bus,
            || self.log_sources(&project, &target, requested.as_deref()),
            req.stderr,
            req.lines,
            &aborted,
            on_chunk,
        )
    }

    /// The retained sources a world currently exposes for `targets` (services first, then hooks).
    ///
    /// A missing world, a stopped service, or a hook that does not exist yet is simply absent: the
    /// follow loop re-resolves this set after every restart epoch.
    fn log_sources(
        &self,
        project: &slipfork_world::ProjectSlug,
        target: &slipfork_world::WorldTarget,
        requested: Option<&str>,
    ) -> Result<Vec<fan_in::LogSource>, EdgeError> {
        let (handle, service_ids, hook_logs) = {
            let state = self.lock();
            let Some(world) = state.world(project, target) else {
                return Ok(Vec::new());
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
        let targets: Vec<String> = match requested {
            Some(name) => vec![name.to_owned()],
            None => service_ids
                .iter()
                .cloned()
                .chain(hook_logs.keys().cloned())
                .collect(),
        };
        let world = world_id(project, target);
        let mut sources = Vec::with_capacity(targets.len());
        for name in &targets {
            if service_ids.iter().any(|id| id == name) {
                // A live service whose window cannot be read is a real failure, never an excuse to
                // silently drop its output: propagate so a one-shot reports it and a follower aborts.
                let log = self
                    .engine
                    .service_logs(&handle, name)
                    .map_err(|err| service_engine_error(&world, name, err))?;
                sources.push(fan_in::LogSource::Service(log));
            } else if let Some(log) = hook_logs.get(name) {
                sources.push(fan_in::LogSource::Hook(Arc::clone(log)));
            }
        }
        Ok(sources)
    }
}
