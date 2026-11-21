//! World lifecycle inside the daemon: boot placeholders, up/down/restart, and teardown.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_ingress::{IngressRegistry, IngressTable};
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::Failure;
use super::rpc::{key_of, pid_alive, world_answers, world_call};
use crate::daemon::registry::{self, RegistryEntry};
use crate::world::HostEnv;

/// One owned world: either still booting, or live with its supervisor child.
pub(super) enum World {
    Booting {
        project: PathBuf,
        world: String,
        supervisor_pid: u32,
    },
    Live {
        entry: RegistryEntry,
        child: Option<Child>,
    },
}

impl World {
    fn name(&self) -> &str {
        match self {
            Self::Booting { world, .. } => world,
            Self::Live { entry, .. } => &entry.world,
        }
    }
}

pub(super) struct Core {
    pub(super) host: HostEnv,
    pub(super) state_dir: PathBuf,
    worlds: BTreeMap<String, World>,
    ingress: Option<(IngressRegistry, BTreeMap<String, PathBuf>)>,
    _lock: File,
}

impl Core {
    pub(super) fn new(
        host: HostEnv,
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
            state_dir,
            worlds,
            ingress: None,
            _lock: lock,
        }
    }

    pub(super) fn world_count(&self) -> usize {
        self.worlds.len()
    }

    /// Every live entry, for the startup rewrite of a filtered registry.
    pub(super) fn retained(&self) -> Vec<RegistryEntry> {
        self.worlds
            .values()
            .filter_map(|world| match world {
                World::Live { entry, .. } => Some(entry.clone()),
                World::Booting { .. } => None,
            })
            .collect()
    }

    fn persist(&self) -> Result<(), Failure> {
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

    /// Spawns `__supervise` and leaves a `Booting` placeholder; the guard is dropped by the caller.
    pub(super) fn begin_up(&mut self, params: &Value) -> Result<(String, Child, PathBuf), Failure> {
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
        let key = key_of(&project);
        match self.worlds.get(&key) {
            Some(World::Booting { world, .. }) => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{world}` is still starting"),
                ));
            }
            Some(World::Live { entry, .. })
                if pid_alive(entry.supervisor_pid) && world_answers(&entry.pods_dir) =>
            {
                return Err(Failure::new(
                    "world-running",
                    format!("world `{}` is already running", entry.world),
                ));
            }
            _ => {}
        }
        self.worlds.remove(&key);
        self.spawn(project, key, params)
    }

    fn spawn(
        &mut self,
        project: PathBuf,
        key: String,
        params: &Value,
    ) -> Result<(String, Child, PathBuf), Failure> {
        let requested = params.get("world").and_then(Value::as_str);
        let world = crate::world::world_name(&project, requested)
            .map_err(|message| Failure::new("invalid-world", message))?;
        let log_dir = project.join(".slipfork");
        fs::create_dir_all(&log_dir).map_err(|error| {
            Failure::new(
                "log-dir",
                format!("create `{}`: {error}", log_dir.display()),
            )
        })?;
        let log_path = log_dir.join("supervisor.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|error| {
                Failure::new(
                    "log-open",
                    format!("open `{}`: {error}", log_path.display()),
                )
            })?;
        let mut child = Command::new(&self.host.exe)
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--world")
            .arg(world.as_str())
            .arg("--json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| {
                Failure::new(
                    "supervisor-spawn",
                    format!("cannot start `__supervise`: {error}"),
                )
            })?;
        child.stdout.take().ok_or_else(|| {
            Failure::new(
                "supervisor-spawn",
                "the world supervisor gave no event stream",
            )
        })?;
        self.worlds.insert(
            key.clone(),
            World::Booting {
                project,
                world: world.as_str().to_owned(),
                supervisor_pid: child.id(),
            },
        );
        Ok((key, child, log_path))
    }

    pub(super) fn publish(
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

    pub(super) fn forget(&mut self, key: &str) {
        self.worlds.remove(key);
        self.ingress = None;
        let _ = self.persist();
    }

    pub(super) fn down(
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
        let key = key_of(&project);
        let Some(world) = self.worlds.get(&key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        if let World::Booting { world, .. } = world {
            return Err(Failure::new(
                "world-booting",
                format!("world `{world}` is still starting"),
            ));
        }
        let name = world.name().to_owned();
        let started = Instant::now();
        emit(json!({
            "event": "started", "step": 1, "total": 2, "kind": "service",
            "label": format!("stopping {name}"),
        }));
        let stopped = self.stop_world(&key, Duration::from_secs(60))?;
        emit(json!({
            "event": "finished", "step": 1, "total": 2, "kind": "service",
            "label": "pods stopped", "detail": "stopped",
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
        emit(json!({
            "event": "stopped", "world": stopped["world"],
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
        self.persist()?;
        Ok(stopped)
    }

    fn stop_world(&mut self, key: &str, timeout: Duration) -> Result<Value, Failure> {
        let Some(mut world) = self.worlds.remove(key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let (project, name, pods_dir, pid, mut child) = match &mut world {
            World::Booting {
                project,
                world,
                supervisor_pid,
                ..
            } => (project.clone(), world.clone(), None, *supervisor_pid, None),
            World::Live { entry, child } => (
                entry.project.clone(),
                entry.world.clone(),
                Some(entry.pods_dir.clone()),
                entry.supervisor_pid,
                child.take(),
            ),
        };
        if let Some(pods_dir) = &pods_dir {
            let shutdown = Request::Shutdown {
                v: PROTOCOL_VERSION,
                id: "daemon-down".to_owned(),
            };
            let _ = world_call(pods_dir, &shutdown);
        }
        wait_exit(pid, child.as_mut(), timeout);
        if pid_alive(pid)
            && let Ok(pid) = i32::try_from(pid)
        {
            slipfork_world::kill_group(pid);
        }
        if let Some(child) = child.as_mut() {
            let _ = child.wait();
        }
        let _ = fs::remove_file(project.join(".slipfork/world.json"));
        self.ingress = None;
        Ok(json!({ "stopped": true, "world": name }))
    }

    pub(super) fn restart(
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
        let subject = params
            .get("service")
            .and_then(Value::as_str)
            .unwrap_or(&world)
            .to_owned();
        let started = Instant::now();
        emit(json!({
            "event": "started", "step": 1, "total": 1, "kind": "service",
            "label": format!("restarting {subject}"),
        }));
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: "daemon-restart".to_owned(),
            service: params
                .get("service")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let reply = world_call(&pods_dir, &request)
            .map_err(|message| Failure::new("restart-failed", message))?;
        emit(json!({
            "event": "finished", "step": 1, "total": 1, "kind": "service",
            "label": "restarted", "detail": format!("{} pod(s)", reply["restarted"]),
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
        self.ingress = None;
        Ok(json!({ "restarted": reply["restarted"], "world": world }))
    }

    pub(super) fn ps(&mut self) -> Result<Value, Failure> {
        let mut worlds = Vec::new();
        for world in self.worlds.values() {
            match world {
                World::Booting {
                    project,
                    world,
                    supervisor_pid,
                    ..
                } => worlds.push(json!({
                    "project": project, "world": world, "fork": null,
                    "supervisor_pid": supervisor_pid, "alive": true,
                    "booting": true, "pods": [],
                })),
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
                    .map(|pods| {
                        pods.into_iter()
                            .map(|pod| json!({ "id": pod.id, "pid": pod.pid, "running": pod.running }))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                    worlds.push(json!({
                        "project": entry.project, "world": entry.world, "fork": entry.fork,
                        "pods_dir": entry.pods_dir, "supervisor_pid": entry.supervisor_pid,
                        "alive": alive, "booting": false, "pods": pods,
                    }));
                }
            }
        }
        Ok(json!({ "worlds": worlds }))
    }

    /// The ingress view, rebuilt only after `up`/`down`/`restart`.
    pub(super) fn ingress_view(
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
            let Ok(document) = slipfork_forkfile::parse(&source) else {
                eprintln!(
                    "slipfork daemon: skipped ingress for `{}`: invalid Forkfile",
                    entry.world
                );
                continue;
            };
            match IngressTable::from_forkfile(entry.world.clone(), &document) {
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

    pub(super) fn stop_all(&mut self) {
        let keys: Vec<String> = self.worlds.keys().cloned().collect();
        for key in keys {
            let _ = self.stop_world(&key, Duration::from_secs(10));
        }
        let _ = self.persist();
    }
}

fn wait_exit(pid: u32, mut child: Option<&mut Child>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(child) = child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => {}
            }
        } else if !pid_alive(pid) {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
