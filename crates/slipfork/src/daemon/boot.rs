//! One boot's event relay: the ledger row, the supervisor stream, and its publication.
//!
//! The daemon owns the supervisor `Child` in [`super::lifecycle::core::World`]; the session only
//! holds its stdout pipe, pid, and the row it publishes, so a rollback can never orphan a process.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ChildStdout;
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_world::LiveFork;
use slipfork_world::WorldName;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::lease::{self, Planned};
use super::now_ms;
use super::rpc::{world_answers, world_call_within};
use super::{Daemon, Failure};

const ABORT_CALL: Duration = Duration::from_secs(5);
const REAP_BUDGET: Duration = Duration::from_secs(5);

/// One in-flight boot, from the reserved ledger row to the published live world.
#[derive(Debug)]
pub(super) struct BootSession {
    /// World-map key the `Booting` entry occupies.
    key: String,
    /// Ledger row this boot publishes.
    world_id: i64,
    /// `supervisor.log` the child's stderr is appended to.
    log: PathBuf,
    /// Supervisor stdout, taken before the `Child` moved into the world map.
    stdout: Option<ChildStdout>,
    /// Supervisor process group leader, retained after the `Child` moves into the map.
    supervisor_pid: u32,
    /// Canonical `<runtime_dir>/slipfork/pods/<world>` tmpfs root for this boot.
    pods_dir: PathBuf,
    /// Project directory the world boots from.
    project: PathBuf,
    /// Store root the published lease is written under.
    store_root: PathBuf,
    /// The world name the ledger row reserved.
    world: String,
    /// The preplanned head and the exact lease `ready` must persist verbatim.
    planned: Planned,
    /// Fork name when this boot is a fork world.
    fork: Option<String>,
    /// The daemon already streamed a merged `plan` frame (a fork's prep rows plus world steps).
    plan_announced: bool,
    /// Idempotence guard so repeated `stop()` calls on fork rollback do not double-poll.
    stopped: bool,
}

impl BootSession {
    /// Builds the session around one already-spawned supervisor.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        key: String,
        world_id: i64,
        log: PathBuf,
        stdout: Option<ChildStdout>,
        supervisor_pid: u32,
        pods_dir: PathBuf,
        project: PathBuf,
        store_root: PathBuf,
        world: String,
        planned: Planned,
    ) -> Self {
        Self {
            key,
            world_id,
            log,
            stdout,
            supervisor_pid,
            pods_dir,
            project,
            store_root,
            world,
            planned,
            fork: None,
            plan_announced: false,
            stopped: false,
        }
    }

    /// Records the fork this boot belongs to, so its failure names the fork checkout.
    pub(super) fn reserve_fork(&mut self, fork: String) {
        self.fork = Some(fork);
    }

    /// Marks the `plan` frame as already streamed, so the supervisor's own frame is not repeated.
    pub(super) fn plan_already_announced(&mut self) {
        self.plan_announced = true;
    }

    /// Relays one boot's typed events, then publishes the live row or settles it `stopped`.
    pub(super) fn run(
        &mut self,
        daemon: &Daemon,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
        let stdout = self.stdout.take().ok_or_else(|| {
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
                let kind = if self.fork.is_some() { "fork" } else { "world" };
                let tail = self.log_tail();
                let reason = if tail.is_empty() {
                    format!(
                        "the {kind} supervisor exited before `{}` was ready",
                        self.world
                    )
                } else {
                    format!(
                        "the {kind} supervisor exited before `{}` was ready:\n{tail}",
                        self.world
                    )
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
            // `ready` is published, not streamed: a ledger write that fails after the world booted
            // must never let the CLI print `✓` for a fork that is about to be rolled back.
            let ready = event["event"].as_str() == Some("ready");
            if !ready {
                emit(event.clone());
            }
            match event["event"].as_str().unwrap_or_default() {
                "ready" => return self.publish(daemon, event, emit),
                "failed" => {
                    let message = event["message"]
                        .as_str()
                        .unwrap_or("the world failed to boot")
                        .to_owned();
                    if let Some(reported) = event["pods_dir"].as_str() {
                        self.pods_dir = PathBuf::from(reported);
                    }
                    // `forget` deletes a fork row and marks a root `stopped`, so a failed boot
                    // never leaves a half-fork that blocks a retry.
                    self.stop();
                    daemon.core()?.forget(&self.key);
                    return Err(Failure::new("boot-failed", message));
                }
                _ => {}
            }
        }
    }

    /// Writes the preplanned lease verbatim, then publishes the row `live`.
    fn publish(
        &mut self,
        daemon: &Daemon,
        event: Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value, Failure> {
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
        let Ok(world_name) = WorldName::parse(&world) else {
            self.fail_boot(daemon)?;
            return Err(Failure::new("world-state", "the world published no name"));
        };
        self.world = world.clone();
        self.pods_dir = pods_dir.clone();
        // The plan was sealed before the boot; a checkout edit during the boot must fail closed
        // instead of publishing a stale head. This probe is pure and runs outside the core lock.
        let drifted = lease::desired_for(&self.project, &world_name)
            .map_or(true, |head| head.spec != self.planned.0.spec);
        if drifted {
            self.stop();
            daemon.core()?.forget(&self.key);
            lease::remove(&self.store_root, &world_name);
            return Err(Failure::new(
                "checkout-drift",
                "the checkout changed while the world was booting\nnext: run `slipfork up` again",
            ));
        }
        // Write the preplanned lease verbatim, never re-deriving it under the lock.
        if let Err(failure) = lease::write(&self.store_root, &world_name, &self.planned.1) {
            self.stop();
            daemon.core()?.forget(&self.key);
            return Err(failure);
        }
        let live = LiveFork {
            supervisor_pid: i64::from(self.supervisor_pid),
            pods_dir,
            started_at_ms: now_ms(),
        };
        if let Err(failure) = daemon.core()?.publish_live(&self.key, self.world_id, live) {
            self.stop();
            daemon.core()?.forget(&self.key);
            return Err(failure);
        }
        // Only a published ledger row may announce `✓ Ready` / `{"event":"ready"}`.
        let url = event["url"].clone();
        emit(event);
        Ok(json!({
            "world": world,
            "url": url,
        }))
    }

    /// Shuts down any live `slipfork-world-init` and reaps `__supervise` plus escaped world groups.
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
        if let Ok(pid) = i32::try_from(self.supervisor_pid) {
            slipfork_world::kill_group(pid);
        }
        slipfork_world::reap_world_tree(self.supervisor_pid, Some(&self.pods_dir), REAP_BUDGET);
    }

    /// Stops an unusable half-boot and settles its row before the failure is reported.
    ///
    /// `forget` prunes the world's run catalog exactly once, so a half-boot never prunes twice.
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
