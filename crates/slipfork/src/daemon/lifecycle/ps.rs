//! `ps`: declared rows folded together with the world's own live pod reply.

use std::fs;
use std::path::{Path, PathBuf};

use slipfork_world::PodStatus;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::rpc::{pid_alive, world_answers, world_call};
use crate::daemon::view;
use crate::ps::{PsKind, PsReply, PsService, PsWorld};

impl Core {
    pub(crate) fn ps(&mut self) -> Result<PsReply, Failure> {
        let mut rows: Vec<(Option<String>, PsWorld)> = Vec::new();
        for world in self.worlds.values() {
            match world {
                World::Booting {
                    project,
                    world,
                    supervisor_pid,
                } => rows.push((
                    None,
                    WorldView::new(project, world, None, 0, *supervisor_pid, true, true, &[])
                        .ps_json(),
                )),
                World::Live { entry, .. } => {
                    let alive = pid_alive(entry.supervisor_pid) && world_answers(&entry.pods_dir);
                    let pods = world_call(
                        &entry.pods_dir,
                        &Request::List {
                            v: PROTOCOL_VERSION,
                            id: "daemon-list".to_owned(),
                        },
                    )
                    .ok()
                    .and_then(|reply| slipfork_world::pod_statuses(&reply).ok())
                    .unwrap_or_default();
                    let parent = entry.fork.as_ref().and_then(|fork| {
                        entry
                            .world
                            .strip_prefix(&format!("{fork}--"))
                            .map(str::to_owned)
                    });
                    rows.push((
                        parent,
                        WorldView::new(
                            &entry.project,
                            &entry.world,
                            entry.fork.as_deref(),
                            started_at(entry),
                            entry.supervisor_pid,
                            alive,
                            false,
                            &pods,
                        )
                        .ps_json(),
                    ));
                }
            }
        }
        let mut worlds: Vec<PsWorld> = rows
            .iter()
            .filter(|(parent, _)| parent.is_none())
            .map(|(_, world)| world.clone())
            .collect();
        for (parent, world) in &rows {
            let Some(parent) = parent else {
                continue;
            };
            match worlds.iter().position(|row| &row.world == parent) {
                Some(index) => worlds[index].forks.push(world.clone()),
                None => worlds.push(world.clone()),
            }
        }
        Ok(PsReply { worlds })
    }
}

/// One world's evaluated rows plus the live facts the daemon can prove.
pub(super) struct WorldView {
    pub(super) project: PathBuf,
    pub(super) world: String,
    pub(super) fork: Option<String>,
    pub(super) started_at_ms: u64,
    pub(super) supervisor_pid: u32,
    pub(super) alive: bool,
    pub(super) booting: bool,
    services: Vec<PsService>,
}

impl WorldView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        project: &Path,
        world: &str,
        fork: Option<&str>,
        started_at_ms: u64,
        supervisor_pid: u32,
        alive: bool,
        booting: bool,
        pods: &[PodStatus],
    ) -> Self {
        let document = fs::read_to_string(project.join("Forkfile"))
            .ok()
            .and_then(|source| slipfork_forkfile::parse(&source).ok());
        let mut services = document.as_ref().map(view::services).unwrap_or_default();
        let s3_ids: Vec<String> = services
            .iter()
            .filter(|svc| svc.kind == PsKind::S3)
            .map(|svc| svc.id.clone())
            .collect();
        let s3_pod = pods
            .iter()
            .find(|pod| s3_ids.iter().any(|id| id == &pod.id));
        for service in &mut services {
            if let Some(pod) = pods.iter().find(|pod| pod.id == service.id) {
                service.pid = Some(pod.pid);
                service.running = alive && pod.running;
            } else if service.kind == PsKind::S3
                && let Some(pod) = s3_pod
            {
                service.pid = Some(pod.pid);
                service.running = alive && pod.running;
            }
        }
        Self {
            project: project.to_path_buf(),
            world: world.to_owned(),
            fork: fork.map(str::to_owned),
            started_at_ms,
            supervisor_pid,
            alive,
            booting,
            services,
        }
    }

    /// The declared services with their live state.
    #[must_use]
    pub(crate) fn service_rows(&self) -> &[PsService] {
        &self.services
    }

    /// The wire object for this world.
    #[must_use]
    pub(crate) fn ps_json(&self) -> PsWorld {
        PsWorld {
            project: self.project.clone(),
            path: self.project.clone(),
            world: self.world.clone(),
            fork: self.fork.clone(),
            url: format!("http://{}.localhost", self.world),
            started_at_ms: self.started_at_ms,
            supervisor_pid: self.supervisor_pid,
            alive: self.alive,
            booting: self.booting,
            services: self.service_rows().to_vec(),
            forks: Vec::new(),
        }
    }
}

/// The world's start time, falling back to the pod root's creation time.
fn started_at(entry: &crate::daemon::registry::RegistryEntry) -> u64 {
    if entry.started_at_ms > 0 {
        return entry.started_at_ms;
    }
    fs::metadata(&entry.pods_dir)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_millis() as u64)
        })
        .unwrap_or(0)
}
