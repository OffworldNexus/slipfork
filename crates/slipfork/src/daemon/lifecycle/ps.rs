//! `ps`: declared rows folded together with the world's own live pod reply.
//!
//! Honesty rules: a missing checkout or an uncompilable `Forkfile` becomes one `problem` row
//! instead of a fabricated empty/orphaned tree, and stopped forks still on disk under
//! `<project>/.forks` stay visible until they are reaped or dropped.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::Diagnostics;
use slipfork_world::PodStatus;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::rpc::{pid_alive, world_answers, world_call};
use crate::daemon::view;
use crate::ps::{PsKind, PsReply, PsService, PsStoppedFork, PsWorld, PsWorldState};

impl Core {
    pub(crate) fn ps(&mut self) -> Result<PsReply, Failure> {
        let mut rows: Vec<(Option<String>, PsWorld)> = Vec::new();
        for world in self.worlds.values() {
            match world {
                World::Booting {
                    project,
                    world,
                    supervisor_pid,
                    ..
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
                        let (parsed, parent) = slipfork_world::StoreLayout::parse_fork_world(
                            OsStr::new(entry.world.as_str()),
                        )?;
                        (parsed.as_str() == fork).then(|| parent.as_str().to_owned())
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
        let mut orphans: Vec<(String, PsWorld)> = Vec::new();
        for (parent, world) in &rows {
            let Some(parent) = parent else {
                continue;
            };
            match find_world_mut(&mut worlds, parent) {
                Some(row) => push_fork(row, world.clone()),
                None => orphans.push((parent.clone(), world.clone())),
            }
        }
        for (parent, fork) in orphans {
            attach_orphan(&mut worlds, &parent, fork);
        }
        let live = live_worlds(&worlds);
        for world in &mut worlds {
            attach_stopped(world, &live);
        }
        Ok(PsReply { worlds })
    }
}

/// Depth-first search for the world registered under `id`.
fn find_world_mut<'a>(worlds: &'a mut [PsWorld], id: &str) -> Option<&'a mut PsWorld> {
    for world in worlds.iter_mut() {
        if world.world == id {
            return Some(world);
        }
        if let Some(found) = find_world_mut(&mut world.forks, id) {
            return Some(found);
        }
    }
    None
}

/// Depth-first search for the world booted from `project`.
fn find_project_mut<'a>(worlds: &'a mut [PsWorld], project: &Path) -> Option<&'a mut PsWorld> {
    for world in worlds.iter_mut() {
        if world.project == project {
            return Some(world);
        }
        if let Some(found) = find_project_mut(&mut world.forks, project) {
            return Some(found);
        }
    }
    None
}

fn push_fork(row: &mut PsWorld, fork: PsWorld) {
    row.forks.push(fork);
    row.forks
        .sort_by(|left, right| left.project.cmp(&right.project));
}

/// Nests a fork whose parent world is no longer registered under the canonical parent project.
///
/// A root world stopped with `slipfork down` leaves its running forks in the registry: without
/// this, they would pose as top-level worlds and every sibling stopped checkout would vanish.
fn attach_orphan(worlds: &mut Vec<PsWorld>, parent: &str, fork: PsWorld) {
    let Some((root, _)) =
        slipfork_world::fork_checkout(&fork.project).filter(|(root, _)| root.is_dir())
    else {
        worlds.push(fork);
        return;
    };
    if let Some(row) = find_project_mut(worlds, &root) {
        push_fork(row, fork);
        return;
    }
    match find_world_mut(worlds, parent) {
        Some(row) => push_fork(row, fork),
        None => {
            let mut placeholder = PsWorld::stopped(&root, parent, None);
            push_fork(&mut placeholder, fork);
            worlds.push(placeholder);
        }
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
    match discover_stopped(&world.project, &world.world, live) {
        Ok(forks) => world.stopped_forks = forks,
        Err(problem) => {
            world.problem = Some(match world.problem.take() {
                Some(existing) => format!("{existing}; {problem}"),
                None => problem,
            });
        }
    }
    for fork in &mut world.forks {
        attach_stopped(fork, live);
    }
}

/// `.forks` subvolume checkouts under `<project>` that no registered world already owns.
///
/// A `read_dir` failure is returned so the caller can print one honest `problem` row instead of an
/// empty family.
fn discover_stopped(
    project: &Path,
    parent_world: &str,
    live: &BTreeSet<String>,
) -> Result<Vec<PsStoppedFork>, String> {
    let forks = slipfork_world::fork_checkouts(project).map_err(|error| error.to_string())?;
    Ok(forks
        .into_iter()
        .filter(|fork| {
            slipfork_world::fork_world_id(parent_world, &fork.name)
                .is_none_or(|id| !live.contains(id.as_str()))
        })
        .map(|fork| PsStoppedFork {
            name: fork.name,
            path: fork.path,
        })
        .collect())
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
    primary_domain: Option<String>,
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
        let (document, mut problem) = evaluate(project, fork, world);
        let features = match &document {
            Some(document) => {
                match crate::world::features::WorldFeatures::load_resolved(project, document) {
                    Ok(features) => Some(features),
                    Err(error) => {
                        problem = Some(error);
                        None
                    }
                }
            }
            None => None,
        };
        let primary_domain = document
            .as_ref()
            .zip(features.as_ref())
            .and_then(|(document, features)| view::primary_domain(document, features));
        let mut services = match (&document, &features) {
            (Some(document), Some(features)) => view::services(document, features),
            _ => pods
                .iter()
                .filter(|pod| pod.running && alive)
                .map(live_service)
                .collect(),
        };
        if features.is_some() {
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
                        command: None,
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
            primary_domain,
            services,
            problem,
        }
    }

    /// The declared or live services, with their live state.
    #[must_use]
    pub(crate) fn service_rows(&self) -> &[PsService] {
        &self.services
    }

    /// The wire object for this live world.
    #[must_use]
    pub(crate) fn ps_json(&self) -> PsWorld {
        PsWorld {
            project: self.project.clone(),
            path: self.project.clone(),
            world: self.world.clone(),
            fork: self.fork.clone(),
            url: format!("http://{}.localhost", self.world),
            primary_domain: self.primary_domain.clone(),
            state: PsWorldState::Live,
            started_at_ms: (self.started_at_ms > 0).then_some(self.started_at_ms),
            supervisor_pid: Some(self.supervisor_pid),
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
        command: None,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use super::{PsWorld, attach_stopped};

    /// A real disk scratch project under `/var/tmp`, removed even when an assertion panics.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(prefix: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let dir =
                PathBuf::from("/var/tmp").join(format!("{prefix}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).expect("create the disk scratch project");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A project that simply has no forks yet must not surface a `problem` row.
    #[test]
    fn a_project_without_a_forks_dir_reports_no_problem() {
        let scratch = Scratch::new("slipfork-ps-no-forks");
        let mut world = PsWorld::stopped(&scratch.0, "demo", None);
        attach_stopped(&mut world, &BTreeSet::new());
        assert!(
            world.problem.is_none(),
            "a missing `.forks` must not become a problem row: {:?}",
            world.problem
        );
        assert!(world.stopped_forks.is_empty());
        assert!(
            slipfork_world::fork_checkouts(&scratch.0)
                .expect("list fork checkouts for a project with no `.forks`")
                .is_empty()
        );
    }
}
