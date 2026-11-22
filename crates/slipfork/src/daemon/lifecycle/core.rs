//! The daemon's world map plus the verbs that only mutate it.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_ingress::{IngressRegistry, IngressTable};
use slipfork_vcs::VcsWorkspace;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::super::Failure;
use super::super::rpc::{key_of, pid_alive, world_answers, world_call};
use crate::daemon::registry::{self, RegistryEntry};
use crate::world::HostEnv;

/// One owned world: either still booting, or live with its supervisor child.
pub(crate) enum World {
    /// Spawned but not published yet.
    Booting {
        /// Project directory the supervisor was spawned for.
        project: PathBuf,
        /// Validated world id.
        world: String,
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
    ) -> Result<(), Failure> {
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
        self.worlds.remove(key);
        self.ingress = None;
        let _ = self.persist();
    }

    pub(crate) fn restart(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
        let entity = self
            .worlds
            .get(&key_of(&project))
            .ok_or_else(|| Failure::new("no-world", "no world is running in this directory"))?;
        let (pods_dir, world, booting) = match entity {
            World::Booting { world, .. } => (None, world.clone(), true),
            World::Live { entry, .. } => (Some(entry.pods_dir.clone()), entry.world.clone(), false),
        };
        if booting {
            return Err(Failure::new(
                "world-booting",
                format!("world `{world}` is still starting"),
            ));
        }
        let pods_dir = pods_dir.expect("a live world has a pod root");
        let service = params
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let subject = service.clone().unwrap_or_else(|| world.clone());
        let started = Instant::now();
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
        Ok(json!({ "restarted": reply["restarted"], "world": world }))
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
            match IngressTable::from_forkfile(entry.world.clone(), &checked.document) {
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
