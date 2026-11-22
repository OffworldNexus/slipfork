//! `ps`: declared rows folded together with the world's own live pod reply.
//!
//! Honesty rules: a missing checkout or an uncompilable `Forkfile` becomes one `problem` row
//! instead of a fabricated empty/orphaned tree, and stopped forks still on disk under
//! `<project>/.forks` stay visible until they are reaped or dropped.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::Diagnostics;
use slipfork_world::PodStatus;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::rpc::{pid_alive, world_answers, world_call};
use crate::daemon::view;
use crate::ps::{PsKind, PsReply, PsService, PsStoppedFork, PsWorld};

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
        let live = live_worlds(&worlds);
        for world in &mut worlds {
            attach_stopped(world, &live);
        }
        Ok(PsReply { worlds })
    }
}

/// Every registered world id, so a discovered `.forks` entry can be skipped when it is live.
fn live_worlds(worlds: &[PsWorld]) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    for world in worlds {
        live.insert(world.world.clone());
        live.extend(live_worlds(&world.forks));
    }
    live
}

/// Attaches every stopped fork found under this world and its nested forks.
fn attach_stopped(world: &mut PsWorld, live: &BTreeSet<String>) {
    world.stopped_forks = discover_stopped(&world.project, &world.world, live);
    for fork in &mut world.forks {
        attach_stopped(fork, live);
    }
}

/// Btrfs subvolume entries under `<project>/.forks` that no registered world already owns.
fn discover_stopped(
    project: &Path,
    parent_world: &str,
    live: &BTreeSet<String>,
) -> Vec<PsStoppedFork> {
    let Ok(entries) = fs::read_dir(project.join(slipfork_world::FORKS_DIR)) else {
        return Vec::new();
    };
    let mut stopped = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if live.contains(&format!("{name}--{parent_world}")) {
            continue;
        }
        if !slipfork_world::is_subvolume_path(&path).unwrap_or(false) {
            continue;
        }
        stopped.push(PsStoppedFork {
            name: name.to_owned(),
            path,
        });
    }
    stopped.sort_by(|left, right| left.name.cmp(&right.name));
    stopped.dedup_by(|left, right| left.name == right.name);
    stopped
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
    problem: Option<String>,
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
        let (document, problem) = evaluate(project, fork, world);
        let mut services = match &document {
            Some(document) => view::services(document),
            None => pods
                .iter()
                .filter(|pod| pod.running && alive)
                .map(live_service)
                .collect(),
        };
        if document.is_some() {
            let declared: Vec<String> = services.iter().map(|service| service.id.clone()).collect();
            for service in &mut services {
                if let Some(pod) = pods.iter().find(|pod| pod.id == service.id) {
                    service.pid = Some(pod.pid);
                    service.running = alive && pod.running;
                } else if service.kind == PsKind::S3 {
                    // Native S3 runs in-process inside the world init: no pod exists to look up,
                    // and the bucket is healthy exactly while its world is up.
                    service.running = alive;
                }
            }
            // UNION the live pods with the declared rows: a pod the edited `Forkfile` no longer
            // declares stays visible (yellow `orphaned`) until `slipfork up` reaps it.
            if alive {
                for pod in pods.iter().filter(|pod| pod.running) {
                    if declared.iter().any(|id| id == &pod.id) {
                        continue;
                    }
                    services.push(PsService {
                        id: pod.id.clone(),
                        kind: PsKind::Service,
                        running: true,
                        port: None,
                        version: None,
                        cdn: None,
                        policy: None,
                        pid: Some(pod.pid),
                        routes: Vec::new(),
                        orphaned: true,
                        image: None,
                    });
                }
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
            problem,
        }
    }

    /// The declared or live services, with their live state.
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
            problem: self.problem.clone(),
            stopped_forks: Vec::new(),
            forks: Vec::new(),
        }
    }
}

/// Reads and compiles the project's `Forkfile`, naming the one honest problem when it cannot.
fn evaluate(
    project: &Path,
    fork: Option<&str>,
    world: &str,
) -> (Option<slipfork_forkfile::Forkfile>, Option<String>) {
    if !project.is_dir() {
        let message = match fork {
            Some(fork) => format!("checkout missing — run `slipfork drop {fork}`"),
            None => format!("checkout missing — restore it or run `slipfork down {world}`"),
        };
        return (None, Some(message));
    }
    match fs::read_to_string(project.join("Forkfile")) {
        Ok(source) => match slipfork_forkfile::compile(&source) {
            Ok(checked) => (Some(*checked.document), None),
            Err(diagnostics) => (None, Some(invalid(&diagnostics))),
        },
        Err(error) => (
            None,
            Some(format!("Forkfile invalid: cannot read Forkfile: {error}")),
        ),
    }
}

/// The first compiler diagnostic as one machine-honest line.
fn invalid(diagnostics: &Diagnostics) -> String {
    diagnostics.first().map_or_else(
        || "Forkfile invalid".to_owned(),
        |item| {
            format!(
                "Forkfile invalid: {}:{}: {}",
                item.span.line(),
                item.span.col(),
                item.message
            )
        },
    )
}

/// One live pod projected as a service row when the `Forkfile` no longer compiles.
fn live_service(pod: &PodStatus) -> PsService {
    PsService {
        id: pod.id.clone(),
        kind: PsKind::Service,
        running: pod.running,
        port: None,
        version: None,
        cdn: None,
        policy: None,
        pid: Some(pod.pid),
        routes: Vec::new(),
        orphaned: false,
        image: None,
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
