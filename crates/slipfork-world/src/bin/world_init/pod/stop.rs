//! Pod stop and restart: concurrent group-kill teardown, one bounded deadline, zero survivors.

use std::fs;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::kill_group;

use super::InitWorld;
use super::launch::{crun_delete, crun_in, crun_run};
use crate::shared::{WorldShared, reap_zombies};
use crate::{Fault, REMEDY_RUNTIME};

/// Wall-clock budget for stopping every pod of one world, shared by every pod thread.
const STOP_BUDGET: Duration = Duration::from_secs(10);

/// One failed teardown step: the pod it belonged to plus its one-line cause.
#[derive(Debug)]
pub(crate) struct StopFault {
    /// Pod id whose teardown failed.
    pub(crate) pod: String,
    /// One-line cause.
    pub(crate) message: String,
}

impl InitWorld {
    /// Restarts one pod or every pod from its existing tmpfs bundle.
    pub(crate) fn restart(&mut self, service: Option<&str>) -> Result<usize, Fault> {
        let ids: Vec<String> = {
            let pids = self
                .shared
                .pids
                .lock()
                .map_err(|_| Fault::new("pod pid lock", REMEDY_RUNTIME))?;
            match service {
                Some(id) if pids.contains_key(id) => vec![id.to_owned()],
                Some(id) => {
                    return Err(Fault::new(
                        format!("pod `{id}` is not running"),
                        "pick a service from `slipfork up` output or omit the service name",
                    ));
                }
                None => pids.keys().cloned().collect(),
            }
        };
        for id in &ids {
            let pid = self
                .shared
                .pids
                .lock()
                .ok()
                .and_then(|pids| pids.get(id).copied());
            let sink = self
                .shared
                .logs
                .lock()
                .ok()
                .and_then(|logs| logs.get(id).cloned());
            let (Some(pid), Some(info), Some(sink)) = (pid, self.shared.info(id), sink) else {
                continue;
            };
            kill_group(pid);
            let _ = reap_pod(pid, Instant::now() + STOP_BUDGET);
            crun_delete(&self.shared, id);
            {
                let mut guard = sink
                    .lock()
                    .map_err(|_| Fault::new(format!("pod `{id}` log lock"), REMEDY_RUNTIME))?;
                guard.divider(id, true).map_err(|source| {
                    Fault::cause(
                        format!("pod `{id}` restart divider"),
                        REMEDY_RUNTIME,
                        source,
                    )
                })?;
            }
            let pid = crun_run(&self.shared, &info.bundle, id, Arc::clone(&sink))?;
            let _ = self.shared.take_status(pid);
            if let Ok(mut pids) = self.shared.pids.lock() {
                pids.insert(id.clone(), pid);
            }
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(row) = pods.get_mut(id)
            {
                row.launches += 1;
            }
        }
        reap_zombies(&self.shared);
        Ok(ids.len())
    }

    /// Stops every pod group concurrently under one deadline, then removes its bundle and logs.
    ///
    /// Each pod gets exactly one `crun kill --all`, one process-group kill, one bounded `waitpid`,
    /// and one `crun delete --force`. A slow or failing pod never delays or abandons its siblings,
    /// and every failure is reported instead of being swallowed.
    pub(crate) fn stop_all(&mut self) {
        self.stop_s3();
        self.shared.mark_stopped();
        for pid in self.shared.exec_pids() {
            kill_group(pid);
        }
        let pods: Vec<(String, i32)> = self
            .shared
            .pids
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard).into_iter().collect())
            .unwrap_or_default();
        let shared = Arc::clone(&self.shared);
        let faults = stop_groups(&pods, STOP_BUDGET, &|id, pid, deadline| {
            stop_one(&shared, id, pid, deadline)
        });
        for fault in faults {
            eprintln!(
                "slipfork-world-init: pod `{}` teardown: {}",
                fault.pod, fault.message
            );
        }
        for (id, _) in &pods {
            if let Ok(mut containers) = self.shared.pods.lock()
                && let Some(container) = containers.remove(id)
            {
                let _ = fs::remove_dir_all(&container.bundle);
            }
        }
        if let Ok(mut logs) = self.shared.logs.lock() {
            logs.clear();
        }
        reap_zombies(&self.shared);
    }
}

/// Runs `stop` for every pod on its own thread under one overall deadline and collects every
/// failure, so no pod is skipped or abandoned when a sibling fails.
fn stop_groups(
    pods: &[(String, i32)],
    budget: Duration,
    stop: &(dyn Fn(&str, i32, Instant) -> Vec<StopFault> + Sync),
) -> Vec<StopFault> {
    let deadline = Instant::now() + budget;
    thread::scope(|scope| {
        let handles: Vec<_> = pods
            .iter()
            .map(|(id, pid)| scope.spawn(move || stop(id, *pid, deadline)))
            .collect();
        let mut faults = Vec::new();
        for ((id, _), handle) in pods.iter().zip(handles) {
            match handle.join() {
                Ok(collected) => faults.extend(collected),
                Err(_) => faults.push(StopFault {
                    pod: id.clone(),
                    message: "teardown thread panicked".to_owned(),
                }),
            }
        }
        faults
    })
}

/// One pod's teardown: `crun kill`, exact process-group kill, bounded reap, `crun delete`.
fn stop_one(shared: &WorldShared, id: &str, pid: i32, deadline: Instant) -> Vec<StopFault> {
    let mut faults = Vec::new();
    if let Err(message) = crun_in(shared, &["kill", "--all", id, "SIGKILL"], deadline) {
        faults.push(StopFault {
            pod: id.to_owned(),
            message,
        });
    }
    kill_group(pid);
    if !reap_pod(pid, deadline) {
        faults.push(StopFault {
            pod: id.to_owned(),
            message: format!("pid {pid} survived its group kill"),
        });
    }
    if let Err(message) = crun_in(shared, &["delete", "--force", id], deadline) {
        faults.push(StopFault {
            pod: id.to_owned(),
            message,
        });
    }
    faults
}

/// Waits until one pod init is reaped after its group kill; `false` when it outlives `deadline`.
fn reap_pod(pid: i32, deadline: Instant) -> bool {
    loop {
        let mut status = 0;
        // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if reaped == pid {
            return true;
        }
        if reaped < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            // `ECHILD`: PID 1's main loop already collected the status; the pod is gone.
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    use super::{STOP_BUDGET, StopFault, kill_group, reap_pod, stop_groups};
    use std::time::Instant;

    /// A real pod-shaped process group: `/bin/sh -c 'sleep 30'` leading its own group.
    fn pod_group() -> Child {
        Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn a real pod group")
    }

    /// The live pids whose process group is `pgid`, read from `/proc/<pid>/stat`.
    fn group_members(pgid: i32) -> Vec<i32> {
        let mut members = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return members;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if pid == std::process::id() as i32 {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some((_, rest)) = stat.rsplit_once(')') else {
                continue;
            };
            let pgrp = rest
                .split_whitespace()
                .nth(2)
                .and_then(|value| value.parse::<i32>().ok());
            if pgrp == Some(pgid) {
                members.push(pid);
            }
        }
        members
    }

    fn alive(pid: i32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    /// Six real pod groups must be in flight at once, and none may survive or leave a zombie.
    #[test]
    fn six_pods_tear_down_concurrently_with_no_survivors_and_no_zombies() {
        let mut pods: Vec<(String, i32)> = Vec::with_capacity(6);
        let mut children = Vec::with_capacity(6);
        for index in 0..6 {
            let child = pod_group();
            pods.push((
                format!("svc-{index}"),
                i32::try_from(child.id()).expect("pid"),
            ));
            children.push(child);
        }

        // Each teardown announces itself and waits for a release token, so a serial loop can
        // never collect all six within the budget.
        let (arrived_tx, arrived_rx) = mpsc::channel::<()>();
        let arrived_tx = Arc::new(Mutex::new(arrived_tx));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let intervals: Arc<Mutex<Vec<(String, Instant, Instant)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let intervals_worker = Arc::clone(&intervals);

        let worker = std::thread::spawn({
            let pods = pods.clone();
            move || {
                stop_groups(&pods, STOP_BUDGET, &|id, pid, deadline| {
                    let started = Instant::now();
                    arrived_tx
                        .lock()
                        .expect("announce lock")
                        .send(())
                        .expect("announce the in-flight teardown");
                    let gate = release_rx.lock().expect("release lock");
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let _ = gate.recv_timeout(remaining);
                    drop(gate);
                    kill_group(pid);
                    let reaped = reap_pod(pid, deadline);
                    if let Ok(mut intervals) = intervals_worker.lock() {
                        intervals.push((id.to_owned(), started, Instant::now()));
                    }
                    if reaped {
                        Vec::new()
                    } else {
                        vec![StopFault {
                            pod: id.to_owned(),
                            message: format!("pid {pid} survived its group kill"),
                        }]
                    }
                })
            }
        });

        for _ in 0..pods.len() {
            arrived_rx
                .recv_timeout(STOP_BUDGET)
                .expect("every pod teardown must be in flight at once");
        }
        for _ in 0..pods.len() {
            release_tx.send(()).expect("release one teardown");
        }
        let faults = worker.join().expect("teardown worker");
        assert!(faults.is_empty(), "{faults:?}");

        let intervals = intervals.lock().expect("interval lock");
        assert_eq!(intervals.len(), 6);
        let first_end = intervals
            .iter()
            .map(|(_, _, end)| *end)
            .min()
            .expect("six intervals");
        assert!(
            intervals.iter().all(|(_, start, _)| *start < first_end),
            "the six teardowns must overlap in wall-clock time: {intervals:?}"
        );

        for (id, pid) in &pods {
            assert!(
                !alive(*pid),
                "pod `{id}` pid {pid} still exists after `down`"
            );
            let members = group_members(*pid);
            assert!(
                members.is_empty(),
                "pod `{id}` group {pid} still has survivors: {members:?}"
            );
        }
        // `reap_pod` collected every status; no defunct child may remain.
        for (id, pid) in &pods {
            let mut status = 0;
            // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
            let reaped = unsafe { libc::waitpid(*pid, &mut status, libc::WNOHANG) };
            assert!(
                reaped == -1,
                "pod `{id}` pid {pid} was not reaped (waitpid returned {reaped})"
            );
        }
        drop(children);
    }
}
