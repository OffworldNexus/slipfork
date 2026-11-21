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
                let tail = self.log_tail();
                let reason = if tail.is_empty() {
                    "the world supervisor exited before the world was ready".to_owned()
                } else {
                    format!("the world supervisor exited before the world was ready:\n{tail}")
                };
                return Err(Failure::new("supervisor-exited", reason));
            }
            let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            emit(event.clone());
            match event["event"].as_str().unwrap_or_default() {
                "ready" => {
                    let Some(pods_dir) = event["pods_dir"].as_str().map(PathBuf::from) else {
                        self.fail_boot(daemon)?;
                        return Err(Failure::new(
                            "world-state",
                            "the world published no pod directory",
                        ));
                    };
                    let Some(world) = event["world"].as_str().filter(|world| !world.is_empty())
                    else {
                        self.fail_boot(daemon)?;
                        return Err(Failure::new("world-state", "the world published no name"));
                    };
                    let entry = RegistryEntry {
                        project: self.project.clone(),
                        world: world.to_owned(),
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
                        "world": world,
                        "url": event["url"],
                    }));
                }
                "failed" => {
                    let message = event["message"]
                        .as_str()
                        .unwrap_or("the world failed to boot")
                        .to_owned();
                    if self.fork.is_none()
                        && let (Some(pods_dir), Some(world), Some(child)) = (
                            event["pods_dir"].as_str().map(PathBuf::from),
                            event["world"].as_str().filter(|world| !world.is_empty()),
                            self.child.take(),
                        )
                    {
                        let entry = RegistryEntry {
                            project: self.project.clone(),
                            world: world.to_owned(),
                            fork: None,
                            pods_dir,
                            supervisor_pid: child.id(),
                            started_at_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |since| since.as_millis() as u64),
                        };
                        daemon.core()?.publish(&self.key, entry, child)?;
                    } else {
                        self.stop();
                        daemon.core()?.forget(&self.key);
                    }
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

    /// Stops an unusable half-boot and drops its placeholder before the failure is reported.
    fn fail_boot(&mut self, daemon: &Daemon) -> Result<(), Failure> {
        self.stop();
        daemon.core()?.forget(&self.key);
        Ok(())
    }

    /// The last ~20 non-empty lines of this boot's supervisor log, so the cause is not lost with
    /// the scratch tree that fork cleanup removes.
    fn log_tail(&self) -> String {
        let Ok(text) = std::fs::read_to_string(&self.log) else {
            return String::new();
        };
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(|line| line.strip_prefix("error: ").unwrap_or(line))
            .collect();
        let start = lines.len().saturating_sub(20);
        lines[start..].join("\n")
    }
}
