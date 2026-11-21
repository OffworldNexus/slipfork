//! Stopping one world: typed shutdown, bounded wait, process-group kill, registry rewrite.

use std::fs;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_world::PodStatus;
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
        let started = Instant::now();
        // The pod roster is read inside `stop_world` before teardown: once the control plane is
        // gone there is nothing left to ask, and every streamed row names a pod the world had.
        let stopped = self.stop_world(&key, Duration::from_secs(60), &mut |pod, millis| {
            emit(json!({
                "event": "finished", "kind": "pod", "name": pod,
                "state": "stopped", "duration_ms": millis, "elapsed_ms": millis,
            }));
        })?;
        // `sweep_ms` is the control plane's own teardown time, measured from the shutdown request
        // until its reply lands; it never re-uses a pod's elapsed time.
        let sweep_ms = stopped["sweep_ms"].as_u64().unwrap_or(0);
        emit(json!({
            "event": "finished", "kind": "removed", "name": "run subvolumes",
            "state": "", "duration_ms": sweep_ms, "elapsed_ms": sweep_ms,
        }));
        emit(json!({
            "event": "stopped", "world": stopped["world"],
            "elapsed_ms": millis(started),
        }));
        self.persist()?;
        Ok(stopped)
    }

    /// The pods the live world reports, read before its control plane is shut down.
    fn known_pods(&self, key: &str) -> Vec<PodStatus> {
        let Some(World::Live { entry, .. }) = self.worlds.get(key) else {
            return Vec::new();
        };
        let roster = Request::List {
            v: PROTOCOL_VERSION,
            id: "daemon-down-list".to_owned(),
        };
        world_call(&entry.pods_dir, &roster)
            .ok()
            .and_then(|reply| slipfork_world::pod_statuses(&reply).ok())
            .unwrap_or_default()
    }

    /// Stops one world, reporting each pod the moment it is observed gone with its own elapsed
    /// time, then the sweep's own duration. `on_pod` receives `(pod id, elapsed millis)`.
    pub(crate) fn stop_world(
        &mut self,
        key: &str,
        timeout: Duration,
        on_pod: &mut dyn FnMut(&str, u64),
    ) -> Result<Value, Failure> {
        let pods = self.known_pods(key);
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
        // Each pod is timed from its own start, so one slow pod never lends its duration to a pod
        // that was already gone when the world began stopping.
        let mut pending: Vec<Pod> = pods.into_iter().map(Pod::new).collect();
        let sweep_started = Instant::now();
        let shutdown = pods_dir.map(|pods_dir| {
            thread::spawn(move || {
                let shutdown = Request::Shutdown {
                    v: PROTOCOL_VERSION,
                    id: "daemon-down".to_owned(),
                };
                let _ = world_call(&pods_dir, &shutdown);
            })
        });
        let deadline = Instant::now() + timeout;
        while !pending.is_empty() && Instant::now() < deadline {
            let mut index = 0;
            while index < pending.len() {
                if pending[index].gone() {
                    let pod = pending.remove(index);
                    on_pod(&pod.id, pod.elapsed());
                    continue;
                }
                index += 1;
            }
            if !pending.is_empty() {
                thread::sleep(Duration::from_millis(50));
            }
        }
        for pod in pending {
            on_pod(&pod.id, pod.elapsed());
        }
        if let Some(shutdown) = shutdown {
            let _ = shutdown.join();
        }
        let sweep_ms = millis(sweep_started);
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
        Ok(json!({ "stopped": true, "world": name, "sweep_ms": sweep_ms }))
    }

    pub(crate) fn stop_all(&mut self) {
        let keys: Vec<String> = self.worlds.keys().cloned().collect();
        for key in keys {
            let _ = self.stop_world(&key, Duration::from_secs(10), &mut |_, _| {});
        }
        let _ = self.persist();
    }
}

/// One pod's own stop clock.
struct Pod {
    id: String,
    pid: i32,
    running: bool,
    started: Instant,
}

impl Pod {
    fn new(status: PodStatus) -> Self {
        Self {
            id: status.id,
            pid: status.pid,
            running: status.running,
            started: Instant::now(),
        }
    }

    fn gone(&self) -> bool {
        !self.running || !u32::try_from(self.pid).is_ok_and(pid_alive)
    }

    fn elapsed(&self) -> u64 {
        millis(self.started)
    }
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
