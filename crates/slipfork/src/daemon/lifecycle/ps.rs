//! `ps`: one row per ledger world, joined to the daemon's owned worlds for liveness.
//!
//! The ledger is authoritative for what exists — roots, live forks, stopped forks, and broken
//! rows all come from `worlds`/`forks`. The daemon only adds the live facts it can prove: an
//! owned supervisor, an answering control socket, and the pods behind it. Untracked checkouts are
//! converged into the ledger before `ps`, so this module never rescans `.forks`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::Diagnostics;
use slipfork_world::control::{PROTOCOL_VERSION, Request};
use slipfork_world::{PodStatus, WorldRow, WorldState};

use super::core::{Core, World, ledger_failure};
use crate::daemon::Failure;
use crate::daemon::rpc::{key_of, pid_alive, world_answers, world_call};
use crate::daemon::view;
use crate::ps::{PsKind, PsReply, PsService, PsWorld, PsWorldState, lineage};

impl Core {
    pub(crate) fn ps(&mut self) -> Result<PsReply, Failure> {
        let rows = self.ledger.list().map_err(ledger_failure)?;
        let known: BTreeSet<i64> = rows.worlds.iter().map(|row| row.id).collect();
        let fork_names: BTreeMap<i64, String> = rows
            .forks
            .iter()
            .map(|fork| (fork.world_id, fork.name.clone()))
            .collect();
        let mut built: BTreeMap<i64, PsWorld> = BTreeMap::new();
        for row in &rows.worlds {
            let (state, alive, booting) = self.liveness(row);
            let started_at_ms = row
                .started_at_ms
                .and_then(|ms| u64::try_from(ms).ok())
                .unwrap_or(0);
            let pid = row
                .supervisor_pid
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or(0);
            let pods = if alive { live_pods(row) } else { Vec::new() };
            let view = WorldView::new(
                &row.checkout_path,
                &row.world_name,
                fork_names.get(&row.id).map(String::as_str),
                started_at_ms,
                pid,
                alive,
                booting,
                &pods,
            );
            let mut world = view.ps_json(state);
            // The ledger's one-line divergence is the only problem a broken row renders.
            world.problem = row.problem.clone().or_else(|| world.problem.take());
            if state == PsWorldState::Broken {
                world.services.clear();
            }
            world.lineage = lineage(&rows, row.id);
            built.insert(row.id, world);
        }
        // `children` is the tree: a fork nests under its parent row, or under its root row when
        // the immediate parent row is gone.
        let mut children: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for fork in &rows.forks {
            let parent = if known.contains(&fork.parent_id) {
                fork.parent_id
            } else if known.contains(&fork.root_id) {
                fork.root_id
            } else {
                continue;
            };
            children.entry(parent).or_default().push(fork.world_id);
        }
        let nested: BTreeSet<i64> = children.values().flatten().copied().collect();
        let mut worlds = Vec::new();
        for row in &rows.worlds {
            if nested.contains(&row.id) {
                continue;
            }
            if let Some(world) = assemble(row.id, &children, &mut built) {
                worlds.push(world);
            }
        }
        // A cycle can never be written by the ledger, but a remainder must still be visible.
        while let Some(id) = built.keys().next().copied() {
            if let Some(world) = assemble(id, &children, &mut built) {
                worlds.push(world);
            }
        }
        Ok(PsReply { worlds })
    }

    /// The ledger row's state joined to the daemon's owned world.
    fn liveness(&self, row: &WorldRow) -> (PsWorldState, bool, bool) {
        let owned = self.worlds.get(&key_of(&row.checkout_path));
        let booting = matches!(owned, Some(World::Booting { .. }));
        let pid = row
            .supervisor_pid
            .and_then(|pid| u32::try_from(pid).ok())
            .unwrap_or(0);
        let alive = matches!(owned, Some(World::Live { .. }))
            && pid != 0
            && pid_alive(pid)
            && row.pods_dir.as_deref().is_some_and(world_answers);
        let state = if row.state == WorldState::Broken {
            PsWorldState::Broken
        } else if alive {
            PsWorldState::Live
        } else {
            PsWorldState::Stopped
        };
        (state, alive, booting)
    }
}

/// Takes one assembled subtree out of `worlds`, nesting its children in id order.
fn assemble(
    id: i64,
    children: &BTreeMap<i64, Vec<i64>>,
    worlds: &mut BTreeMap<i64, PsWorld>,
) -> Option<PsWorld> {
    let mut world = worlds.remove(&id)?;
    if let Some(kids) = children.get(&id) {
        for &kid in kids {
            if let Some(child) = assemble(kid, children, worlds) {
                world.forks.push(child);
            }
        }
    }
    world
        .forks
        .sort_by(|left, right| left.project.cmp(&right.project));
    Some(world)
}

/// The running pods a live world reports over its control socket.
fn live_pods(row: &WorldRow) -> Vec<PodStatus> {
    let Some(dir) = row.pods_dir.as_deref() else {
        return Vec::new();
    };
    world_call(
        dir,
        &Request::List {
            v: PROTOCOL_VERSION,
            id: "daemon-list".to_owned(),
        },
    )
    .ok()
    .and_then(|reply| slipfork_world::pod_statuses(&reply).ok())
    .unwrap_or_default()
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

    /// The wire object for this world under its authoritative lifecycle `state`.
    #[must_use]
    pub(crate) fn ps_json(&self, state: PsWorldState) -> PsWorld {
        PsWorld {
            project: self.project.clone(),
            path: self.project.clone(),
            world: self.world.clone(),
            fork: self.fork.clone(),
            url: format!("http://{}.localhost", self.world),
            primary_domain: self.primary_domain.clone(),
            state,
            started_at_ms: (self.started_at_ms > 0).then_some(self.started_at_ms),
            supervisor_pid: (self.supervisor_pid > 0).then_some(self.supervisor_pid),
            alive: self.alive,
            booting: self.booting,
            services: self.service_rows().to_vec(),
            problem: self.problem.clone(),
            stopped_forks: Vec::new(),
            forks: Vec::new(),
            lineage: Vec::new(),
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
