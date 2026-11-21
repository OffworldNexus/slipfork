//! Stopping one world: typed shutdown, bounded wait, process-group kill, registry rewrite.

use std::fs;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::rpc::{key_of, pid_alive, world_call};

impl Core {
    pub(crate) fn down(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let project = std::path::PathBuf::from(
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

    pub(crate) fn stop_world(&mut self, key: &str, timeout: Duration) -> Result<Value, Failure> {
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

    pub(crate) fn stop_all(&mut self) {
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
