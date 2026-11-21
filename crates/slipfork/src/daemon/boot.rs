//! One boot's event relay: the reserved world key, the supervisor stream, and its publication.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Child;

use serde_json::{Value, json};

use super::registry::RegistryEntry;
use super::{Daemon, Failure};

fn kill_and_wait(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        slipfork_world::kill_group(pid);
    }
    let _ = child.wait();
}

/// One in-flight boot, from the reserved registry key to the published live world.
#[derive(Debug)]
pub(super) struct BootSession {
    /// Registry key the placeholder already occupies.
    key: String,
    /// `supervisor.log` the child's stderr is appended to.
    log: PathBuf,
    /// Supervised child; `None` once the live registry entry owns it.
    child: Option<Child>,
    /// Project directory the world boots from.
    project: PathBuf,
    /// Fork name when this boot is a fork world.
    fork: Option<String>,
}

impl BootSession {
    /// Builds the session around one already-spawned supervisor.
    pub(super) fn new(key: String, log: PathBuf, child: Child, project: PathBuf) -> Self {
        Self {
            key,
            log,
            child: Some(child),
            project,
            fork: None,
        }
    }

    /// Records the fork this boot belongs to, so its published entry nests under the parent world.
    pub(super) fn reserve_fork(&mut self, fork: String) {
        self.fork = Some(fork);
    }

    /// Relays one boot's typed events, then publishes the live entry or forgets the placeholder.
    pub(super) fn run(
        &mut self,
        daemon: &Daemon,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let stdout = self
            .child
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or_else(|| {
                Failure::new(
                    "supervisor-spawn",
                    "the world supervisor gave no event stream",
                )
            })?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).map_err(|error| {
                Failure::new(
                    "supervisor-read",
                    format!("cannot read boot events: {error}"),
                )
            })?;
            if read == 0 {
                self.stop();
                daemon.core()?.forget(&self.key);
                return Err(Failure::new(
                    "supervisor-exited",
                    format!(
                        "the world supervisor exited before the world was ready (see `{}`)",
                        self.log.display()
                    ),
                ));
            }
            let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            emit(event.clone());
            match event["event"].as_str().unwrap_or_default() {
                "ready" => {
                    let pods_dir =
                        event["pods_dir"]
                            .as_str()
                            .map(PathBuf::from)
                            .ok_or_else(|| {
                                Failure::new("world-state", "the world published no pod directory")
                            })?;
                    let entry = RegistryEntry {
                        project: self.project.clone(),
                        world: event["world"].as_str().unwrap_or_default().to_owned(),
                        fork: self.fork.clone(),
                        pods_dir,
                        supervisor_pid: self.child.as_ref().map_or(0, Child::id),
                        started_at_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |since| since.as_millis() as u64),
                    };
                    let child = self.child.take().ok_or_else(|| {
                        Failure::new("supervisor-spawn", "the world supervisor disappeared")
                    })?;
                    daemon.core()?.publish(&self.key, entry, child)?;
                    return Ok(json!({
                        "world": event["world"],
                        "url": event["url"],
                    }));
                }
                "failed" => {
                    self.stop();
                    daemon.core()?.forget(&self.key);
                    let message = event["message"]
                        .as_str()
                        .unwrap_or("the world failed to boot")
                        .to_owned();
                    return Err(Failure::new("boot-failed", message));
                }
                _ => {}
            }
        }
    }

    /// Kills the supervisor's whole process group and reaps it, so no pipe outlives the boot.
    fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            kill_and_wait(child);
        }
    }
}
