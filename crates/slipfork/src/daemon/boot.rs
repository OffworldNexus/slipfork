//! One boot's event relay: the reserved world key, the supervisor stream, and its publication.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::registry::RegistryEntry;
use super::rpc::{world_answers, world_call_within};
use super::{Daemon, Failure};

const ABORT_CALL: Duration = Duration::from_secs(5);
const ABORT_EXIT: Duration = Duration::from_secs(2);
const REAP_BUDGET: Duration = Duration::from_secs(5);

/// One in-flight boot, from the reserved registry key to the published live world.
#[derive(Debug)]
pub(super) struct BootSession {
    /// Registry key the placeholder already occupies.
    key: String,
    /// `supervisor.log` the child's stderr is appended to.
    log: PathBuf,
    /// Supervised child; `None` once published or stopped.
    child: Option<Child>,
    /// Supervisor pid retained after `child` is taken so rollback can still reap its session.
    supervisor_pid: u32,
    /// Canonical `<runtime_dir>/slipfork/pods/<world>` tmpfs root for this boot.
    pods_dir: PathBuf,
    /// Project directory the world boots from.
    project: PathBuf,
    /// Fork name when this boot is a fork world.
    fork: Option<String>,
    /// The daemon already streamed a merged `plan` frame (a fork's prep rows plus world steps).
    plan_announced: bool,
    /// Idempotence guard so repeated `stop()` calls on fork rollback do not double-poll.
    stopped: bool,
}

impl BootSession {
    /// Builds the session around one already-spawned supervisor.
    pub(super) fn new(
        key: String,
        log: PathBuf,
        child: Child,
        pods_dir: PathBuf,
        project: PathBuf,
    ) -> Self {
        let supervisor_pid = child.id();
        Self {
            key,
            log,
            child: Some(child),
            supervisor_pid,
            pods_dir,
            project,
            fork: None,
            plan_announced: false,
            stopped: false,
        }
    }

    /// Records the fork this boot belongs to, so its published entry nests under the parent world.
    pub(super) fn reserve_fork(&mut self, fork: String) {
        self.fork = Some(fork);
    }

    /// Marks the `plan` frame as already streamed, so the supervisor's own frame is not repeated.
    pub(super) fn plan_already_announced(&mut self) {
        self.plan_announced = true;
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
            // A fork already streamed one merged plan frame (prep rows + world steps), so the
            // supervisor's own frame would only be a duplicate width announcement.
            if self.plan_announced && event["event"].as_str() == Some("plan") {
                continue;
            }
            // `ready` is published, not streamed: a registry write that fails after the world
            // booted must never let the CLI print `✓` for a fork that is about to be rolled back.
            let ready = event["event"].as_str() == Some("ready");
            if !ready {
                emit(event.clone());
            }
            match event["event"].as_str().unwrap_or_default() {
                "ready" => {
                    let Some(pods_dir) = event["pods_dir"].as_str().map(PathBuf::from) else {
                        self.fail_boot(daemon)?;
                        return Err(Failure::new(
                            "world-state",
                            "the world published no pod directory",
                        ));
                    };
                    let Some(world) = event["world"]
                        .as_str()
                        .filter(|world| !world.is_empty())
                        .map(str::to_owned)
                    else {
                        self.fail_boot(daemon)?;
                        return Err(Failure::new("world-state", "the world published no name"));
                    };
                    let entry = RegistryEntry {
                        project: self.project.clone(),
                        world: world.clone(),
                        fork: self.fork.clone(),
                        pods_dir,
                        supervisor_pid: self.supervisor_pid,
                        started_at_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |since| since.as_millis() as u64),
                    };
                    let child = self.child.take().ok_or_else(|| {
                        Failure::new("supervisor-spawn", "the world supervisor disappeared")
                    })?;
                    daemon.core()?.publish(&self.key, entry, child)?;
                    // Only a persisted registry entry may announce `✓ Ready` / `{"event":"ready"}`.
                    let url = event["url"].clone();
                    emit(event);
                    return Ok(json!({
                        "world": world,
                        "url": url,
                    }));
                }
                "failed" => {
                    let message = event["message"]
                        .as_str()
                        .unwrap_or("the world failed to boot")
                        .to_owned();
                    if let Some(reported) = event["pods_dir"].as_str() {
                        self.pods_dir = PathBuf::from(reported);
                    }
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
                            supervisor_pid: self.supervisor_pid,
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

    /// Shuts down any live `slipfork-world-init`, reaps `__supervise` plus escaped world groups,
    /// and cleans `.slipfork/world.json` so no half-booted world or pipe outlives the session.
    pub(super) fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if world_answers(&self.pods_dir) {
            let shutdown = Request::Shutdown {
                v: PROTOCOL_VERSION,
                id: "daemon-boot-abort".to_owned(),
            };
            let _ = world_call_within(&self.pods_dir, &shutdown, ABORT_CALL);
        }
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + ABORT_EXIT;
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            if let Ok(pid) = i32::try_from(self.supervisor_pid) {
                slipfork_world::kill_group(pid);
            }
            let _ = child.wait();
        }
        slipfork_world::reap_world_tree(self.supervisor_pid, Some(&self.pods_dir), REAP_BUDGET);
        let _ = std::fs::remove_file(self.project.join(".slipfork/world.json"));
    }

    /// Stops an unusable half-boot and drops its placeholder before the failure is reported.
    ///
    /// `forget` prunes the world's run catalog exactly once, so a half-boot never prunes twice.
    fn fail_boot(&mut self, daemon: &Daemon) -> Result<(), Failure> {
        self.stop();
        let mut core = daemon.core()?;
        core.forget(&self.key);
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
