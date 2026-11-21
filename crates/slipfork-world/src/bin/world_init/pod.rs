//! Launched containers inside the world: `crun run`, captured logs, waits, restart, teardown.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;
use slipfork_world::control::FrameLines;
use slipfork_world::pod_log::{CHUNK, LogKind, LogSink};

use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

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
    pods_root: PathBuf,
    pods: Mutex<BTreeMap<String, PodInfo>>,
    /// Exit statuses reaped by the main loop, retained for an `exec` child that races it.
    reaped: Mutex<BTreeMap<i32, i32>>,
    exec_pids: Mutex<Vec<i32>>,
    stop: AtomicBool,
}

impl WorldShared {
    pub(crate) fn crun(&self) -> &Path {
        &self.crun
    }

    pub(crate) fn pods_root(&self) -> &Path {
        &self.pods_root
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

    fn exec_pids(&self) -> Vec<i32> {
        self.exec_pids
            .lock()
            .map(|pids| pids.clone())
            .unwrap_or_default()
    }
}

pub(crate) struct InitWorld {
    shared: std::sync::Arc<WorldShared>,
    /// Open append sinks; the descriptor stays in the init across restarts.
    logs: BTreeMap<String, std::sync::Arc<Mutex<LogSink>>>,
    pids: BTreeMap<String, i32>,
}

impl InitWorld {
    pub(crate) fn new(crun: PathBuf, pods_root: PathBuf) -> Self {
        Self {
            shared: std::sync::Arc::new(WorldShared {
                crun,
                pods_root,
                pods: Mutex::new(BTreeMap::new()),
                reaped: Mutex::new(BTreeMap::new()),
                exec_pids: Mutex::new(Vec::new()),
                stop: AtomicBool::new(false),
            }),
            logs: BTreeMap::new(),
            pids: BTreeMap::new(),
        }
    }

    pub(crate) fn shared(&self) -> std::sync::Arc<WorldShared> {
        std::sync::Arc::clone(&self.shared)
    }

    pub(crate) fn pods_root(&self) -> &Path {
        self.shared.pods_root()
    }

    /// `<pods>/logs`.
    fn logs_dir(&self) -> PathBuf {
        self.shared.pods_root().join("logs")
    }

    /// Every tracked pod with its pid and whether `/proc/<pid>` still exists in the world.
    pub(crate) fn list(&self) -> Vec<serde_json::Value> {
        self.pids
            .iter()
            .map(|(id, pid)| {
                serde_json::json!({
                    "id": id,
                    "pid": pid,
                    "running": Path::new(&format!("/proc/{pid}")).exists(),
                })
            })
            .collect()
    }

    /// Bounded wait: `Some(code)` when the pod exited, `None` while it is still running.
    pub(crate) fn wait(&self, pid: i32, timeout_ms: u64) -> Result<Option<i32>, Fault> {
        if !self.pids.values().any(|candidate| *candidate == pid) {
            return Err(Fault::new(
                format!("pid {pid} is not a launched pod"),
                "pass the pid returned by the launch reply, or restart the world",
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let mut status = 0;
            // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid {
                return Ok(Some(exit_code(status)));
            }
            if reaped < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    if let Some(code) = self.shared.take_status(pid) {
                        return Ok(Some(code));
                    }
                    return Err(Fault::new(
                        format!("pod pid {pid} was already reaped"),
                        "query `crun list` for the container state, then `slipfork restart`",
                    ));
                }
                return Err(Fault::cause(
                    format!("wait for pod pid {pid}"),
                    REMEDY_POD,
                    error,
                ));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn launch(&mut self, pod: &PodSpec) -> Result<i32, Fault> {
        let action = format!("pod `{}` launch with crun", pod.id);
        pod.validate()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_POD, source))?;
        let bundle = self.pods_root().join(&pod.id);
        let bundle = canonical(&bundle, action.clone(), REMEDY_RUNTIME)?;
        if !bundle.starts_with(self.pods_root()) {
            return Err(Fault::new(
                action,
                "keep the pod runtime root on a tmpfs owned by this user",
            ));
        }
        let sink = LogSink::open(&self.logs_dir(), &pod.id)
            .map_err(|source| Fault::cause(action.clone(), REMEDY_RUNTIME, source))?;
        let restarted = self.logs.contains_key(&pod.id);
        let sink = std::sync::Arc::new(Mutex::new(sink));
        {
            let mut guard = sink
                .lock()
                .map_err(|_| Fault::new(format!("pod `{}` log lock", pod.id), REMEDY_RUNTIME))?;
            guard
                .divider(&pod.id, restarted)
                .map_err(|source| Fault::cause(action.clone(), REMEDY_RUNTIME, source))?;
        }
        let log = sink
            .lock()
            .map_err(|_| Fault::new(format!("pod `{}` log lock", pod.id), REMEDY_RUNTIME))?
            .path()
            .to_path_buf();
        let pid = crun_run(
            &self.shared.crun,
            &bundle,
            &pod.id,
            std::sync::Arc::clone(&sink),
        )?;
        self.logs.insert(pod.id.clone(), sink);
        self.pids.insert(pod.id.clone(), pid);
        if let Ok(mut pods) = self.shared.pods.lock() {
            pods.insert(
                pod.id.clone(),
                PodInfo {
                    bundle,
                    workdir: pod.workdir.clone(),
                    log,
                    launches: 1,
                },
            );
        }
        Ok(pid)
    }

    /// Restarts one pod or every pod from its existing tmpfs bundle.
    pub(crate) fn restart(&mut self, service: Option<&str>) -> Result<usize, Fault> {
        let ids: Vec<String> = match service {
            Some(id) if self.pids.contains_key(id) => vec![id.to_owned()],
            Some(id) => {
                return Err(Fault::new(
                    format!("pod `{id}` is not running"),
                    "pick a service from `slipfork up` output or omit the service name",
                ));
            }
            None => self.pids.keys().cloned().collect(),
        };
        for id in &ids {
            let Some(&pid) = self.pids.get(id) else {
                continue;
            };
            let Some(info) = self.shared.info(id) else {
                continue;
            };
            let Some(sink) = self.logs.get(id).cloned() else {
                continue;
            };
            slipfork_world::kill_group(pid);
            self.delete(id);
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
            let pid = crun_run(
                &self.shared.crun,
                &info.bundle,
                id,
                std::sync::Arc::clone(&sink),
            )?;
            self.pids.insert(id.clone(), pid);
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(row) = pods.get_mut(id)
            {
                row.launches += 1;
            }
        }
        reap_zombies(&self.shared);
        Ok(ids.len())
    }

    pub(crate) fn stop_all(&mut self) {
        self.shared.mark_stopped();
        for pid in self.shared.exec_pids() {
            slipfork_world::kill_group(pid);
        }
        for (id, pid) in std::mem::take(&mut self.pids) {
            self.delete(&id);
            slipfork_world::kill_group(pid);
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(container) = pods.remove(&id)
            {
                let _ = fs::remove_dir_all(&container.bundle);
            }
        }
        self.logs.clear();
        reap_zombies(&self.shared);
    }

    /// Runs one bundle to completion while forwarding every stdout/stderr line.
    ///
    /// Unlike `launch` this never detaches: the container's pipes stay open, so a failing `uv sync`
    /// reports its own stderr instead of vanishing behind an exit code.
    ///
    /// `timeout_ms` is enforced here, inside PID 1. The init loop is single-threaded, so a silent
    /// step must never block it: on expiry the process group is killed, the container is deleted,
    /// zombies are reaped, and a typed timeout fault is returned. The `on_line` early return path
    /// runs the same teardown so an undeliverable reply never orphans a container.
    pub(crate) fn run_once_streaming(
        &self,
        pod: &PodSpec,
        timeout_ms: u64,
        on_line: &mut dyn FnMut(slipfork_world::Stream, &str) -> Result<(), Fault>,
    ) -> Result<i32, Fault> {
        let bundle = self.pods_root().join(&pod.id);
        if !bundle.starts_with(self.pods_root()) {
            return Err(Fault::new(
                format!("pod `{}` bundle escapes the runtime root", pod.id),
                REMEDY_CRUN,
            ));
        }
        let action = format!("pod `{}` run-once with crun", pod.id);
        let mut child = Command::new(&self.shared.crun)
            .args(["--cgroup-manager=disabled", "run", "--bundle"])
            .arg(&bundle)
            .arg(&pod.id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
        let child_pid = child.id();
        let (sender, receiver) = mpsc::channel::<(slipfork_world::Stream, String)>();
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let sender = sender.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = sender.send((slipfork_world::Stream::Stdout, line));
                }
            }));
        }
        if let Some(stderr) = child.stderr.take() {
            let sender = sender.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = sender.send((slipfork_world::Stream::Stderr, line));
                }
            }));
        }
        drop(sender);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.abort(&pod.id, child_pid, &mut child, &mut readers);
                return Err(Fault::timed_out(
                    format!(
                        "pod `{}` still running after {timeout_ms}ms; killed its process group",
                        pod.id
                    ),
                    REMEDY_POD,
                ));
            }
            match receiver.recv_timeout(remaining) {
                Ok((stream, line)) => {
                    if let Err(fault) = on_line(stream, &line) {
                        self.abort(&pod.id, child_pid, &mut child, &mut readers);
                        return Err(fault);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        for (reader, kind) in readers.into_iter().zip(["stdout", "stderr"]) {
            if reader.join().is_err() {
                self.delete(&pod.id);
                reap_zombies(&self.shared);
                return Err(Fault::new(
                    format!("pod `{}` {kind} reader panicked", pod.id),
                    REMEDY_CRUN,
                ));
            }
        }
        let status = child
            .wait()
            .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))?;
        self.delete(&pod.id);
        reap_zombies(&self.shared);
        Ok(status.code().unwrap_or(128))
    }

    /// Kills the whole run-once group, deletes the container, reaps, and joins its readers.
    fn abort(
        &self,
        id: &str,
        pid: u32,
        child: &mut std::process::Child,
        readers: &mut Vec<thread::JoinHandle<()>>,
    ) {
        if let Ok(pid) = i32::try_from(pid) {
            slipfork_world::kill_group(pid);
        }
        self.delete(id);
        let _ = child.wait();
        reap_zombies(&self.shared);
        for reader in readers.drain(..) {
            let _ = reader.join();
        }
    }

    /// `crun kill --all` then `crun delete --force`; both are best effort teardown.
    fn delete(&self, id: &str) {
        let _ = Command::new(&self.shared.crun)
            .args(["--cgroup-manager=disabled", "kill", "--all", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new(&self.shared.crun)
            .args(["--cgroup-manager=disabled", "delete", "--force", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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

/// Runs one already-written bundle to container start and returns its host pid.
///
/// The container's stdout/stderr stay piped, and one pump per stream appends timestamped lines to
/// the pod's capture file, so two pods merge chronologically and secrets never reach disk.
fn crun_run(
    crun: &Path,
    bundle: &Path,
    id: &str,
    sink: std::sync::Arc<Mutex<LogSink>>,
) -> Result<i32, Fault> {
    let action = format!("pod `{id}` launch with crun");
    let pid_file = bundle.join("init.pid");
    let _ = fs::remove_file(&pid_file);
    let mut child = Command::new(crun)
        // `--cgroup-manager` is a crun global option: it must precede the subcommand.
        .args(["--cgroup-manager=disabled", "run", "--detach", "--bundle"])
        .arg(bundle)
        .arg("--pid-file")
        .arg(&pid_file)
        .arg(id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
    if let Some(stdout) = child.stdout.take() {
        spawn_pump(stdout, std::sync::Arc::clone(&sink));
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_pump(stderr, std::sync::Arc::clone(&sink));
    }
    let status = child
        .wait()
        .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
    if !status.success() {
        return Err(Fault::new(
            format!("pod `{id}` launch with crun (`crun run` exited with {status})"),
            REMEDY_CRUN,
        ));
    }
    fs::read_to_string(&pid_file)
        .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?
        .trim()
        .parse::<i32>()
        .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))
}

/// Appends every line of one container stream to the timestamped capture.
fn spawn_pump<R: Read + Send + 'static>(mut reader: R, sink: std::sync::Arc<Mutex<LogSink>>) {
    thread::spawn(move || {
        let mut lines = FrameLines::new();
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    for raw in lines.feed(&chunk[..read], CHUNK) {
                        append(&sink, &raw);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if let Some(raw) = lines.take_partial() {
            append(&sink, &raw);
        }
    });
}

fn append(sink: &Mutex<LogSink>, raw: &[u8]) {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches('\r');
    if let Ok(mut sink) = sink.lock() {
        let _ = sink.append(LogKind::Line, text);
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
