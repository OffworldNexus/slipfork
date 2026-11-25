//! The daemon's world map plus the verbs that only mutate it.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_ingress::{IngressRegistry, IngressTable};
use slipfork_vcs::VcsWorkspace;
use slipfork_world::control::{PROTOCOL_VERSION, Request};
use slipfork_world::{LogStore, WorldName};

use super::super::Failure;
use super::super::rpc::{key_of, pid_alive, world_answers, world_call};
use super::plan;
use crate::daemon::registry::{self, Fingerprint, RegistryEntry};
use crate::world::HostEnv;

/// One owned world: either still booting, or live with its supervisor child.
pub(crate) enum World {
    /// Spawned but not published yet.
    Booting {
        /// Project directory the supervisor was spawned for.
        project: PathBuf,
        /// Validated world id.
        world: String,
        /// Canonical tmpfs pod root (`<runtime_dir>/slipfork/pods/<world>`).
        pods_dir: PathBuf,
        /// Supervisor process id.
        supervisor_pid: u32,
    },
    /// Published and reachable through its pod control socket.
    Live {
        /// Durable registry record.
        entry: RegistryEntry,
        /// Supervisor child, when this daemon spawned it.
        child: Option<Child>,
    },
}

impl World {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Booting { world, .. } => world,
            Self::Live { entry, .. } => &entry.world,
        }
    }
}

/// What one `restart` must do, decided under the daemon lock and executed outside it.
pub(crate) enum RestartAction {
    /// The live world still matches the recorded head: restart its pods in place.
    Restarted(Value),
    /// The head changed on disk: pre-plan images, stop the live world, then boot the edited plan.
    Reconcile,
}

/// The daemon's mutable world state.
pub(crate) struct Core {
    pub(super) host: HostEnv,
    /// The canonical `specs/03` §5 branch/dirty port, injected by the composition root.
    pub(super) vcs: Arc<dyn VcsWorkspace>,
    pub(super) state_dir: PathBuf,
    pub(super) worlds: BTreeMap<String, World>,
    pub(super) ingress: Option<(IngressRegistry, BTreeMap<String, PathBuf>)>,
    _lock: File,
}

impl Core {
    pub(crate) fn new(
        host: HostEnv,
        vcs: Arc<dyn VcsWorkspace>,
        state_dir: PathBuf,
        lock: File,
        entries: Vec<RegistryEntry>,
    ) -> Self {
        let mut worlds = BTreeMap::new();
        for entry in entries {
            if pid_alive(entry.supervisor_pid) && world_answers(&entry.pods_dir) {
                worlds.insert(key_of(&entry.project), World::Live { entry, child: None });
            }
        }
        Self {
            host,
            vcs,
            state_dir,
            worlds,
            ingress: None,
            _lock: lock,
        }
    }

    pub(crate) fn world_count(&self) -> usize {
        self.worlds.len()
    }

    /// The injected host state; an image RPC clones it before releasing the core lock.
    pub(crate) fn host(&self) -> &HostEnv {
        &self.host
    }

    /// Every live entry, for the startup rewrite of a filtered registry.
    pub(crate) fn retained(&self) -> Vec<RegistryEntry> {
        self.worlds
            .values()
            .filter_map(|world| match world {
                World::Live { entry, .. } => Some(entry.clone()),
                World::Booting { .. } => None,
            })
            .collect()
    }

    pub(crate) fn persist(&self) -> Result<(), Failure> {
        let entries: Vec<RegistryEntry> = self
            .worlds
            .values()
            .filter_map(|world| match world {
                World::Live { entry, .. } => Some(entry.clone()),
                World::Booting { .. } => None,
            })
            .collect();
        registry::store(&self.state_dir, &entries).map_err(|error| {
            Failure::new(
                "registry-write",
                format!("could not write worlds.json: {error}"),
            )
        })
    }

    pub(crate) fn publish(
        &mut self,
        key: &str,
        entry: RegistryEntry,
        child: Child,
        recorded: Option<Fingerprint>,
    ) -> Result<(), Failure> {
        let _ = registry::store_fingerprint(&self.state_dir, &entry.project, recorded.as_ref());
        self.worlds.insert(
            key.to_owned(),
            World::Live {
                entry,
                child: Some(child),
            },
        );
        self.ingress = None;
        self.persist()
    }

    pub(crate) fn forget(&mut self, key: &str) {
        if let Some(world) = self.worlds.remove(key) {
            let (project, name) = match world {
                World::Booting { project, world, .. } => (project, world),
                World::Live { entry, .. } => (entry.project, entry.world),
            };
            Self::report_retain(self.retain_runs(&project, &name));
            let _ = registry::store_fingerprint(&self.state_dir, &project, None);
        }
        self.ingress = None;
        let _ = self.persist();
    }

    /// The canonical `<store>/logs/<world>` store for `project`, live or stopped.
    pub(crate) fn log_store(&self, project: &Path) -> Option<LogStore> {
        let world = match self.worlds.get(&key_of(project)) {
            Some(world) => world.name().to_owned(),
            None => crate::world::world_name(project, None)
                .ok()?
                .as_str()
                .to_owned(),
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
        current: Option<Fingerprint>,
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
            World::Booting { world, .. } => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{world}` is still starting"),
                ));
            }
            World::Live { entry, .. } => entry.world.clone(),
        };
        // The same staleness check `up` uses: an edited `Forkfile`, `Dockerfile`, or copied
        // context stops the old world and boots the new plan instead of restarting stale pods.
        if self.world_stale(&project, current) {
            // The caller detaches only after the daemon sealed the edited images.
            return Ok(RestartAction::Reconcile);
        }
        let Some(World::Live { entry, .. }) = self.worlds.get(&key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let pods_dir = entry.pods_dir.clone();
        let service = params
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let subject = service.clone().unwrap_or_else(|| world.clone());
        let started = Instant::now();
        // Every declared step is announced before the first row so the CLI measures its six
        // columns once; `restart` itself is one operation, hence `total = 1`.
        plan::emit(&plan::world_rows(&project), 1, emit);
        emit(json!({
            "event": "started", "step": 1, "total": 1, "kind": "restarting",
            "label": subject,
        }));
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: "daemon-restart".to_owned(),
            service: service.clone(),
        };
        let reply = world_call(&pods_dir, &request)
            .map_err(|message| Failure::new("restart-failed", message))?;
        // Exactly one finished node, and only for a real `-s` target: an all-services restart is
        // a world-wide operation, so labelling the world a `service` would fabricate a component.
        if let Some(service) = &service {
            emit(json!({
                "event": "finished", "step": 1, "total": 1, "kind": "service",
                "name": service, "state": "running",
                "label": format!("Restarted {service}"),
                "detail": format!("{} pod(s)", reply["restarted"]),
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "duration_ms": started.elapsed().as_millis() as u64,
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
            let World::Live { entry, .. } = world else {
                continue;
            };
            let Ok(source) = fs::read_to_string(entry.project.join("Forkfile")) else {
                continue;
            };
            let Ok(checked) = slipfork_forkfile::compile(&source) else {
                eprintln!(
                    "slipfork daemon: skipped ingress for `{}`: invalid Forkfile",
                    entry.world
                );
                continue;
            };
            let features = match crate::world::features::WorldFeatures::load_resolved(
                &entry.project,
                &checked.document,
            ) {
                Ok(features) => features,
                Err(error) => {
                    eprintln!(
                        "slipfork daemon: skipped ingress for `{}`: {error}",
                        entry.world
                    );
                    continue;
                }
            };
            match IngressTable::from_forkfile_with(
                entry.world.clone(),
                &checked.document,
                &features,
            ) {
                Ok(table) => {
                    pods.insert(entry.world.clone(), entry.pods_dir.clone());
                    tables.push(table);
                }
                Err(error) => {
                    eprintln!(
                        "slipfork daemon: skipped ingress for `{}`: {error}",
                        entry.world
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
