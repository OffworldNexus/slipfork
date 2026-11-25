//! The daemon's world map plus the verbs that only mutate it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_ingress::{IngressRegistry, IngressTable};
use slipfork_vcs::VcsWorkspace;
use slipfork_world::control::{PROTOCOL_VERSION, Request};
use slipfork_world::{ForkLedger, LedgerError, LiveFork, LogStore, WorldName, WorldRow};

use super::super::Failure;
use super::super::now_ms;
use super::super::rpc::{key_of, world_call};
use super::plan;
use crate::daemon::lease::Head;
use crate::world::HostEnv;

/// One owned world: either still booting, or live with its supervisor child.
pub(crate) enum World {
    /// Spawned but not published yet.
    Booting {
        /// Ledger row reserved for this boot.
        row: WorldRow,
        /// Supervisor child owned by the daemon until it publishes.
        child: Child,
    },
    /// Published and reachable through its pod control socket.
    Live {
        /// Durable ledger row.
        row: WorldRow,
        /// Supervisor child, when this daemon spawned it.
        child: Option<Child>,
    },
}

impl World {
    pub(crate) fn name(&self) -> &str {
        &self.row().world_name
    }

    pub(crate) fn row(&self) -> &WorldRow {
        match self {
            Self::Booting { row, .. } | Self::Live { row, .. } => row,
        }
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.row()
            .supervisor_pid
            .and_then(|pid| u32::try_from(pid).ok())
    }

    /// Reaps an exited supervisor without blocking on a live one.
    pub(crate) fn reap_if_exited(self) {
        let child = match self {
            Self::Booting { child, .. } => Some(child),
            Self::Live { child, .. } => child,
        };
        if let Some(mut child) = child {
            let _ = child.try_wait();
        }
    }
}

/// What one `restart` must do, decided under the daemon lock and executed outside it.
pub(crate) enum RestartAction {
    /// The live world still leases the desired head: restart its pods in place.
    Restarted(Value),
    /// The head changed on disk: pre-plan images, stop the live world, then boot the edited plan.
    Reconcile,
}

/// The daemon's mutable world state.
pub(crate) struct Core {
    pub(crate) host: HostEnv,
    /// The canonical `specs/03` §5 branch/dirty port, injected by the composition root.
    pub(super) vcs: Arc<dyn VcsWorkspace>,
    pub(crate) ledger: ForkLedger,
    pub(crate) worlds: BTreeMap<String, World>,
    pub(super) ingress: Option<(IngressRegistry, BTreeMap<String, PathBuf>)>,
}

impl Core {
    pub(crate) fn new(host: HostEnv, vcs: Arc<dyn VcsWorkspace>, ledger: ForkLedger) -> Self {
        Self {
            host,
            vcs,
            ledger,
            worlds: BTreeMap::new(),
            ingress: None,
        }
    }

    pub(crate) fn world_count(&self) -> usize {
        self.worlds.len()
    }

    /// The injected host state; an image RPC clones it before releasing the core lock.
    pub(crate) fn host(&self) -> &HostEnv {
        &self.host
    }

    /// The ledger row whose checkout is exactly `project`.
    pub(crate) fn row_for_project(&self, project: &Path) -> Option<WorldRow> {
        self.ledger
            .list()
            .ok()?
            .worlds
            .into_iter()
            .find(|row| row.checkout_path == project)
    }

    /// The root row whose checkout is exactly `project`; a fork row is never a root.
    pub(crate) fn root_row(&self, project: &Path) -> Option<WorldRow> {
        let rows = self.ledger.list().ok()?;
        let forks: BTreeSet<i64> = rows.forks.iter().map(|fork| fork.world_id).collect();
        rows.worlds
            .into_iter()
            .find(|row| row.checkout_path == project && !forks.contains(&row.id))
    }

    pub(crate) fn row_by_id(&self, id: i64) -> Option<WorldRow> {
        self.ledger
            .list()
            .ok()?
            .worlds
            .into_iter()
            .find(|row| row.id == id)
    }

    pub(crate) fn is_fork_row(&self, id: i64) -> bool {
        self.ledger
            .list()
            .is_ok_and(|rows| rows.forks.iter().any(|fork| fork.world_id == id))
    }

    /// The fork name recorded for `world_id`, when the row is a fork.
    pub(crate) fn fork_name(&self, world_id: i64) -> Option<String> {
        self.ledger
            .list()
            .ok()?
            .forks
            .into_iter()
            .find(|fork| fork.world_id == world_id)
            .map(|fork| fork.name)
    }

    /// The immediate parent world of a forked checkout, as the lease `origin`.
    pub(crate) fn origin_world(&self, project: &Path) -> Option<WorldName> {
        let rows = self.ledger.list().ok()?;
        let row = rows
            .worlds
            .iter()
            .find(|row| row.checkout_path == project)?;
        let parent = crate::ps::ancestry(&rows, row.id)
            .first()
            .copied()
            .and_then(|id| rows.worlds.iter().find(|row| row.id == id))?;
        WorldName::parse(&parent.world_name).ok()
    }

    /// Publishes `live` in the ledger, then swaps the booting entry for a live one.
    pub(crate) fn publish_live(
        &mut self,
        key: &str,
        world_id: i64,
        live: LiveFork,
    ) -> Result<(), Failure> {
        self.ledger
            .publish_live(world_id, live, now_ms())
            .map_err(ledger_failure)?;
        let row = self
            .row_by_id(world_id)
            .ok_or_else(|| Failure::new("ledger", "the published world row disappeared"))?;
        let child = match self.worlds.remove(key) {
            Some(World::Booting { child, .. }) => Some(child),
            Some(World::Live { child, .. }) => child,
            None => None,
        };
        self.worlds
            .insert(key.to_owned(), World::Live { row, child });
        self.ingress = None;
        Ok(())
    }

    /// Drops an in-memory entry and settles its ledger row: forks are deleted, roots stopped.
    pub(crate) fn forget(&mut self, key: &str) {
        if let Some(world) = self.worlds.remove(key) {
            let row = world.row().clone();
            self.remove_lease(&row.checkout_path, &row.world_name);
            Self::report_retain(self.retain_runs(&row.checkout_path, &row.world_name));
            let outcome = if self.is_fork_row(row.id) {
                self.ledger.delete_world_row(row.id)
            } else {
                self.ledger.mark_stopped(row.id, now_ms())
            };
            if let Err(error) = outcome {
                eprintln!(
                    "slipfork daemon: {}",
                    error.to_string().lines().next().unwrap_or_default()
                );
            }
            // `BootSession::stop`/`stop_world` already killed the `-pgid` group, so the child is
            // only waited on here; dropping the handle un-waited would leave a zombie.
            let child = match world {
                World::Booting { child, .. } => Some(child),
                World::Live { child, .. } => child,
            };
            if let Some(mut child) = child {
                let _ = child.wait();
            }
        }
        self.ingress = None;
    }

    /// The canonical `<store>/logs/<world>` store for `project`, live or stopped.
    pub(crate) fn log_store(&self, project: &Path) -> Option<LogStore> {
        let world = match self.worlds.get(&key_of(project)) {
            Some(world) => world.name().to_owned(),
            None => self
                .row_for_project(project)
                .map(|row| row.world_name)
                .or_else(|| {
                    crate::world::world_name(project, None)
                        .ok()
                        .map(|name| name.as_str().to_owned())
                })?,
        };
        Some(LogStore::at(
            self.host.store_root_for(project),
            WorldName::parse(&world).ok()?,
        ))
    }

    /// Prunes one explicit world's captures to [`slipfork_world::RETAIN_RUNS`] successes plus every
    /// failure, returning how many were removed.
    ///
    /// A `failed` record and its `.log`/`.idx` are never pruned: the artifact an operator needs
    /// after a failed boot or a `down` must outlive every teardown.
    pub(crate) fn retain_runs(&self, project: &Path, world: &str) -> Result<usize, Failure> {
        let store = LogStore::at(
            self.host.store_root_for(project),
            WorldName::parse(world).map_err(|error| Failure::new("no-world", error.to_string()))?,
        );
        store.retain(slipfork_world::RETAIN_RUNS).map_err(|error| {
            Failure::new(
                "run-retain",
                format!(
                    "cannot prune run captures in `{}`: {error}",
                    store.run_dir().display()
                ),
            )
        })
    }

    /// Reports a prune failure on stderr without failing the teardown that triggered it.
    pub(super) fn report_retain(outcome: Result<usize, Failure>) {
        if let Err(failure) = outcome {
            eprintln!(
                "slipfork daemon: {}",
                failure.message.lines().next().unwrap_or_default()
            );
        }
    }

    pub(crate) fn restart(
        &mut self,
        params: &Value,
        current: Option<&Head>,
        emit: &mut dyn FnMut(Value),
    ) -> Result<RestartAction, Failure> {
        let project = super::up::project_path(params)?;
        let key = key_of(&project);
        let Some(entity) = self.worlds.get(&key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let world = match entity {
            World::Booting { row, .. } => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{}` is still starting", row.world_name),
                ));
            }
            World::Live { row, .. } => row.world_name.clone(),
        };
        // The same head check `up` uses: an edited `Forkfile`, feature set, `Dockerfile`, or
        // copied context stops the old world and boots the new plan instead of restarting stale
        // pods. An unchanged head restarts both pods in place on the images already leased.
        if !self.head_matches(&project, &world, current) {
            // The caller detaches only after the daemon sealed the edited images.
            return Ok(RestartAction::Reconcile);
        }
        let Some(World::Live { row, .. }) = self.worlds.get(&key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let Some(pods_dir) = row.pods_dir.clone() else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let service = params
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let subject = service.clone().unwrap_or_else(|| world.clone());
        let started = Instant::now();
        // Every declared step is announced before the first row so the CLI measures its six
        // columns once; `restart` itself is one operation, hence `total = 1`.
        plan::emit(&plan::world_rows(&project), 1, emit);
        // Learn which pods this call will recycle before it runs, so the settled row can name
        // each one instead of leaving a dangling `→ restarting` header.
        let mut targets: Vec<String> = world_call(
            &pods_dir,
            &Request::List {
                v: PROTOCOL_VERSION,
                id: "daemon-restart-list".to_owned(),
            },
        )
        .ok()
        .and_then(|reply| slipfork_world::pod_statuses(&reply).ok())
        .map(|pods| {
            pods.into_iter()
                .filter(|pod| pod.running)
                .filter(|pod| service.as_ref().is_none_or(|id| id == &pod.id))
                .map(|pod| pod.id)
                .collect()
        })
        .unwrap_or_default();
        if let Some(service) = &service
            && !targets.iter().any(|id| id == service)
        {
            targets.push(service.clone());
        }
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: "daemon-restart".to_owned(),
            service: service.clone(),
        };
        let reply = world_call(&pods_dir, &request)
            .map_err(|message| Failure::new("restart-failed", message))?;
        let elapsed = started.elapsed().as_millis() as u64;
        emit(json!({
            "event": "finished", "step": 1, "total": 1, "kind": "restarting",
            "name": subject, "state": "restarted",
            "elapsed_ms": elapsed, "duration_ms": elapsed,
        }));
        for id in targets {
            emit(json!({
                "event": "finished", "kind": "service", "name": id,
                "state": "running", "elapsed_ms": 0, "duration_ms": 0,
            }));
        }
        self.ingress = None;
        Ok(RestartAction::Restarted(
            json!({ "restarted": reply["restarted"], "world": world }),
        ))
    }

    /// The ingress view, rebuilt only after `up`/`down`/`restart`.
    pub(crate) fn ingress_view(
        &mut self,
    ) -> Result<(IngressRegistry, BTreeMap<String, PathBuf>), Failure> {
        if let Some(view) = &self.ingress {
            return Ok(view.clone());
        }
        let mut tables = Vec::new();
        let mut pods = BTreeMap::new();
        for world in self.worlds.values() {
            let World::Live { row, .. } = world else {
                continue;
            };
            let Ok(source) = fs::read_to_string(row.checkout_path.join("Forkfile")) else {
                continue;
            };
            let Ok(checked) = slipfork_forkfile::compile(&source) else {
                eprintln!(
                    "slipfork daemon: skipped ingress for `{}`: invalid Forkfile",
                    row.world_name
                );
                continue;
            };
            let features = match crate::world::features::WorldFeatures::load_resolved(
                &row.checkout_path,
                &checked.document,
            ) {
                Ok(features) => features,
                Err(error) => {
                    eprintln!(
                        "slipfork daemon: skipped ingress for `{}`: {error}",
                        row.world_name
                    );
                    continue;
                }
            };
            match IngressTable::from_forkfile_with(
                row.world_name.clone(),
                &checked.document,
                &features,
            ) {
                Ok(table) => {
                    if let Some(dir) = &row.pods_dir {
                        pods.insert(row.world_name.clone(), dir.clone());
                    }
                    tables.push(table);
                }
                Err(error) => {
                    eprintln!(
                        "slipfork daemon: skipped ingress for `{}`: {error}",
                        row.world_name
                    );
                }
            }
        }
        let registry = IngressRegistry::new(tables)
            .map_err(|error| Failure::new("ingress-table", error.to_string()))?;
        let view = (registry, pods);
        self.ingress = Some(view.clone());
        Ok(view)
    }
}

/// One ledger refusal as a daemon `Failure`; the Display already carries exactly one `next:`.
pub(crate) fn ledger_failure(error: LedgerError) -> Failure {
    Failure::new("ledger", error.to_string())
}
