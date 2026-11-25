//! Stopping one world: typed shutdown, bounded wait, process-group kill, registry rewrite.

use std::fs;
use std::path::Path;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request};
use slipfork_world::{LedgerError, PodStatus, RuntimePods, WorldName};

use super::core::{Core, World, ledger_failure};
use super::plan;
use crate::daemon::Failure;
use crate::daemon::now_ms;
use crate::daemon::rpc::{key_of, pid_alive, world_answers, world_call};

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
        if let World::Booting { row, .. } = world {
            return Err(Failure::new(
                "world-booting",
                format!("world `{}` is still starting", row.world_name),
            ));
        }
        let started = Instant::now();
        // The plan frame is announced before the first row: every pod the world reports, plus the
        // final run-subvolume sweep row this verb always prints.
        let mut rows: Vec<plan::PlannedRow> = self
            .known_pods(&key)
            .into_iter()
            .map(|pod| ("pod".to_owned(), pod.id))
            .collect();
        rows.push(("removed".to_owned(), "run subvolumes".to_owned()));
        plan::emit(&rows, rows.len(), emit);
        // The phase header names the world: one row per pod follows, then the final verdict.
        emit(json!({
            "event": "started", "kind": "stopping", "name": world.name(),
            "step": 1, "total": 1,
        }));
        // The pod roster is read inside `stop_world` before teardown: once the control plane is
        // gone there is nothing left to ask, and every streamed row names a pod the world had.
        let stopped = self.stop_world(&key, Duration::from_secs(60), &mut |pod, millis| {
            emit(json!({
                "event": "finished", "kind": "pod", "name": pod,
                "state": "stopped", "duration_ms": millis, "elapsed_ms": millis,
            }));
        })?;
        // `sweep_ms` is the control plane's own teardown time, measured from the shutdown request
        // until the supervisor sweeps ephemeral pod rootfs clones and exits.
        let sweep_ms = stopped["sweep_ms"].as_u64().unwrap_or(0);
        emit(json!({
            "event": "finished", "kind": "removed", "name": "run subvolumes",
            "state": "", "duration_ms": sweep_ms, "elapsed_ms": sweep_ms,
        }));
        emit(json!({
            "event": "stopped", "world": stopped["world"],
            "elapsed_ms": millis(started),
        }));
        Ok(stopped)
    }

    /// The pods the live world reports, read before its control plane is shut down.
    fn known_pods(&self, key: &str) -> Vec<PodStatus> {
        let Some(World::Live { row, .. }) = self.worlds.get(key) else {
            return Vec::new();
        };
        let Some(pods_dir) = row.pods_dir.as_deref() else {
            return Vec::new();
        };
        let roster = Request::List {
            v: PROTOCOL_VERSION,
            id: "daemon-down-list".to_owned(),
        };
        world_call(pods_dir, &roster)
            .ok()
            .and_then(|reply| slipfork_world::pod_statuses(&reply).ok())
            .unwrap_or_default()
    }

    /// Stops one world through `Request::Shutdown` on `<pods>/control.sock`.
    ///
    /// Pod PIDs reported by `List` belong to the child PID namespace (where `2..50` alias host
    /// `kworker` threads!), so host `/proc/<pod_pid>` must NEVER be polled here: `world_init`
    /// kills, `crun delete`s, and `waitpid`s every pod inside the namespace before answering
    /// `Shutdown`.
    pub(crate) fn stop_world(
        &mut self,
        key: &str,
        timeout: Duration,
        on_pod: &mut dyn FnMut(&str, u64),
    ) -> Result<Value, Failure> {
        let pods = self.known_pods(key);
        let Some(world) = self.worlds.remove(key) else {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        };
        let id = world.row().id;
        let project = world.row().checkout_path.clone();
        let name = world.row().world_name.clone();
        let recorded_pods = world.row().pods_dir.clone();
        let runtime_dir = self.host.runtime_dir.clone();
        let pods_dir = recorded_pods.or_else(|| {
            WorldName::parse(&name)
                .ok()
                .map(|world| RuntimePods::world_root(&runtime_dir, &world))
        });
        let (pid, mut child) = match world {
            World::Booting { row, child } => (
                row.supervisor_pid.and_then(|pid| u32::try_from(pid).ok()),
                Some(child),
            ),
            World::Live { row, child } => (
                row.supervisor_pid.and_then(|pid| u32::try_from(pid).ok()),
                child,
            ),
        };
        let pid = pid.unwrap_or(0);
        if let Err(error) = self.ledger.mark_stopping(id, now_ms())
            && !matches!(error, LedgerError::UnknownRow { .. })
        {
            return Err(ledger_failure(error));
        }
        let pod_started = Instant::now();
        let roster = PodRoster::new(pods);
        // Pods that already exited answer at `t = 0`, before the shutdown request is sent.
        roster.report_exited(on_pod);
        let sweep_started = Instant::now();
        if let Some(dir) = pods_dir.as_deref().filter(|dir| world_answers(dir)) {
            let shutdown = Request::Shutdown {
                v: PROTOCOL_VERSION,
                id: "daemon-down".to_owned(),
            };
            let _ = world_call(dir, &shutdown);
        }
        let pod_ms = millis(pod_started);
        roster.report_live(on_pod, pod_ms);
        wait_exit(pid, child.as_mut(), timeout);
        if pid_alive(pid)
            && let Ok(pid) = i32::try_from(pid)
        {
            slipfork_world::kill_group(pid);
        }
        if let Some(child) = child.as_mut() {
            let _ = child.wait();
        }
        // Reap any `unshare`/`slipfork-world-init`/`slirp4netns` groups that escaped
        // `__supervise`'s pgid via `.process_group(0)` and wait until `/proc` proves none survive.
        slipfork_world::reap_world_tree(pid, pods_dir.as_deref(), Duration::from_secs(5));
        // The init removes its own tmpfs root on a clean exit; a killed supervisor can leave
        // bundles and ephemeral subvolume mounts behind, so `down` sweeps the run root too.
        let swept = sweep_run_root(pods_dir.as_deref());
        let sweep_ms = millis(sweep_started);
        // A stopped world keeps its newest successes and every failed run, so `logs -r failed`
        // still works after `down`.
        Self::report_retain(self.retain_runs(&project, &name));
        self.remove_lease(&project, &name);
        self.ledger
            .mark_stopped(id, now_ms())
            .map_err(ledger_failure)?;
        self.ingress = None;
        Ok(json!({
            "stopped": true,
            "world": name,
            "sweep_ms": sweep_ms,
            "swept": swept,
        }))
    }

    pub(crate) fn stop_all(&mut self) {
        let keys: Vec<String> = self.worlds.keys().cloned().collect();
        for key in keys {
            let _ = self.stop_world(&key, Duration::from_secs(10), &mut |_, _| {});
        }
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
        thread::sleep(Duration::from_millis(15));
    }
}

/// The pod rows one `down` emits: pods that already exited answer at `t = 0`, live pods are
/// reported together once the world has swept them.
struct PodRoster {
    exited: Vec<String>,
    live: Vec<String>,
}

impl PodRoster {
    fn new(pods: Vec<PodStatus>) -> Self {
        let mut roster = Self {
            exited: Vec::new(),
            live: Vec::new(),
        };
        for pod in pods {
            if pod.running {
                roster.live.push(pod.id);
            } else {
                roster.exited.push(pod.id);
            }
        }
        roster
    }

    /// Emits one row at `t = 0` for every pod whose process had already exited.
    fn report_exited(&self, on_pod: &mut dyn FnMut(&str, u64)) {
        for id in &self.exited {
            on_pod(id, 0);
        }
    }

    /// Emits one row per pod the world stopped, stamped with the teardown duration.
    fn report_live(&self, on_pod: &mut dyn FnMut(&str, u64), elapsed_ms: u64) {
        for id in &self.live {
            on_pod(id, elapsed_ms);
        }
    }
}

/// Removes the world's tmpfs run root; `true` when anything was actually swept.
fn sweep_run_root(pods_dir: Option<&Path>) -> bool {
    pods_dir.is_some_and(|dir| fs::remove_dir_all(dir).is_ok())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use slipfork_world::PodStatus;

    use super::{PodRoster, sweep_run_root};

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn pod(id: &str, pid: i32, running: bool) -> PodStatus {
        PodStatus {
            id: id.to_owned(),
            pid,
            running,
            exit_code: (!running).then_some(0),
        }
    }

    /// One `down` reports every pod exactly once, and sweeps the world's run root.
    #[test]
    fn down_reports_every_pod_and_removes_the_run_root() {
        let roster = PodRoster::new(vec![
            pod("pg", 11, true),
            pod("cache", 12, true),
            pod("api", 13, true),
            pod("worker", 14, true),
            pod("hook-seed", 15, false),
            pod("hook-migrate", 16, false),
        ]);
        let mut rows: Vec<(String, u64)> = Vec::new();
        roster.report_exited(&mut |id, millis| rows.push((id.to_owned(), millis)));
        roster.report_live(&mut |id, millis| rows.push((id.to_owned(), millis)), 42);

        let mut ids: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            ["api", "cache", "hook-migrate", "hook-seed", "pg", "worker"],
            "every pod must be reported exactly once"
        );
        for (id, millis) in &rows {
            let expected = if id.starts_with("hook-") { 0 } else { 42 };
            assert_eq!(*millis, expected, "`{id}` was stamped at the wrong instant");
        }

        let root = temp("slipfork-down-run-root");
        let run_root = root.path().join("runtime/slipfork/pods/demo");
        fs::create_dir_all(run_root.join("api")).expect("create a pod bundle");
        fs::write(run_root.join("api/config.json"), b"{}").expect("write a bundle");
        assert!(
            sweep_run_root(Some(&run_root)),
            "`down` must sweep the run root"
        );
        assert!(!run_root.exists(), "no run subvolume may outlive `down`");
        assert!(
            !sweep_run_root(Some(&run_root)),
            "a swept run root reports nothing to remove"
        );
        assert!(!sweep_run_root(None), "a booting world has no run root");
    }
}
