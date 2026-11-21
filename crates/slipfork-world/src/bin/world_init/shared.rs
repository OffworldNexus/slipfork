//! Shared world state between PID 1's main loop and detached streaming (`logs`/`exec`) threads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Immutable facts a stream thread needs about one running pod.
#[derive(Debug, Clone)]
pub(crate) struct PodInfo {
    /// tmpfs bundle directory (`config.json` lives here).
    pub bundle: PathBuf,
    /// Absolute in-container working directory.
    pub workdir: String,
    /// `<pods>/logs/<id>.log`.
    pub log: PathBuf,
    /// How many times this pod was launched (1 = started, >1 = restarted).
    pub launches: usize,
}

/// State shared between the init loop and the streaming threads it spawns.
pub(crate) struct WorldShared {
    crun: PathBuf,
    crun_root: PathBuf,
    pods_root: PathBuf,
    pub(crate) pods: Mutex<BTreeMap<String, PodInfo>>,
    /// Exit statuses reaped by the main loop, retained for an `exec` child that races it.
    reaped: Mutex<BTreeMap<i32, i32>>,
    exec_pids: Mutex<Vec<i32>>,
    exec_seq: AtomicU64,
    stop: AtomicBool,
}

impl WorldShared {
    pub(crate) fn new(crun: PathBuf, crun_root: PathBuf, pods_root: PathBuf) -> Self {
        Self {
            crun,
            crun_root,
            pods_root,
            pods: Mutex::new(BTreeMap::new()),
            reaped: Mutex::new(BTreeMap::new()),
            exec_pids: Mutex::new(Vec::new()),
            exec_seq: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    /// Builds a `crun` invocation isolated to this world's tmpfs state directory.
    pub(crate) fn crun_command(&self) -> Command {
        let mut command = Command::new(&self.crun);
        // `--root` and `--cgroup-manager` are crun global options: they must precede subcommands.
        command
            .arg("--root")
            .arg(&self.crun_root)
            .arg("--cgroup-manager=disabled");
        command
    }

    pub(crate) fn pods_root(&self) -> &Path {
        &self.pods_root
    }

    pub(crate) fn next_exec_seq(&self) -> u64 {
        self.exec_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn info(&self, id: &str) -> Option<PodInfo> {
        self.pods.lock().ok()?.get(id).cloned()
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.pods
            .lock()
            .map(|pods| pods.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_stopped(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(crate) fn record(&self, pid: i32, status: i32) {
        if let Ok(mut reaped) = self.reaped.lock() {
            reaped.insert(pid, status);
        }
    }

    pub(crate) fn take_status(&self, pid: i32) -> Option<i32> {
        self.reaped.lock().ok()?.remove(&pid)
    }

    pub(crate) fn register_exec(&self, pid: i32) {
        if let Ok(mut pids) = self.exec_pids.lock() {
            pids.push(pid);
        }
    }

    pub(crate) fn unregister_exec(&self, pid: i32) {
        if let Ok(mut pids) = self.exec_pids.lock() {
            pids.retain(|candidate| *candidate != pid);
        }
    }

    pub(crate) fn exec_pids(&self) -> Vec<i32> {
        self.exec_pids
            .lock()
            .map(|pids| pids.clone())
            .unwrap_or_default()
    }
}

/// Normalizes a raw `waitpid(2)` status into a shell-style exit code.
pub(crate) fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    }
}

/// Reaps every exited child without blocking, recording each status for a racing `exec` waiter.
pub(crate) fn reap_zombies(shared: &WorldShared) {
    loop {
        let mut status = 0;
        // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
        let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return;
        }
        shared.record(reaped, exit_code(status));
    }
}
