//! Launched containers inside the world: `crun run`, captured logs, waits, restart, teardown.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;
use slipfork_world::control::FrameLines;
use slipfork_world::pod_log::{CHUNK, LogKind, LogSink};

pub(crate) use crate::shared::{PodInfo, WorldShared, exit_code, reap_zombies};
use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

pub(crate) struct InitWorld {
    shared: Arc<WorldShared>,
    /// Open append sinks; the descriptor stays in the init across restarts.
    logs: BTreeMap<String, Arc<Mutex<LogSink>>>,
    pids: BTreeMap<String, i32>,
}

impl InitWorld {
    pub(crate) fn new(crun: PathBuf, pods_root: PathBuf) -> Result<Self, Fault> {
        let crun_root = pods_root.join("crun");
        fs::create_dir_all(&crun_root)
            .and_then(|()| fs::set_permissions(&crun_root, fs::Permissions::from_mode(0o700)))
            .map_err(|source| Fault::cause("crun state root setup", REMEDY_RUNTIME, source))?;
        Ok(Self {
            shared: Arc::new(WorldShared::new(crun, crun_root, pods_root)),
            logs: BTreeMap::new(),
            pids: BTreeMap::new(),
        })
    }

    pub(crate) fn shared(&self) -> Arc<WorldShared> {
        Arc::clone(&self.shared)
    }

    pub(crate) fn pods_root(&self) -> &Path {
        self.shared.pods_root()
    }

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
        let bundle = canonical(
            &self.pods_root().join(&pod.id),
            action.clone(),
            REMEDY_RUNTIME,
        )?;
        if !bundle.starts_with(self.pods_root()) {
            return Err(Fault::new(
                action,
                "keep the pod runtime root on a tmpfs owned by this user",
            ));
        }
        let sink = LogSink::open(&self.logs_dir(), &pod.id)
            .map_err(|source| Fault::cause(action.clone(), REMEDY_RUNTIME, source))?;
        let restarted = self.logs.contains_key(&pod.id);
        let log = sink.path().to_path_buf();
        let sink = Arc::new(Mutex::new(sink));
        {
            let mut guard = sink
                .lock()
                .map_err(|_| Fault::new(format!("pod `{}` log lock", pod.id), REMEDY_RUNTIME))?;
            guard
                .divider(&pod.id, restarted)
                .map_err(|source| Fault::cause(action, REMEDY_RUNTIME, source))?;
        }
        let pid = crun_run(&self.shared, &bundle, &pod.id, Arc::clone(&sink))?;
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
            let (Some(&pid), Some(info), Some(sink)) = (
                self.pids.get(id),
                self.shared.info(id),
                self.logs.get(id).cloned(),
            ) else {
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
            let pid = crun_run(&self.shared, &info.bundle, id, Arc::clone(&sink))?;
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
        let mut child = self
            .shared
            .crun_command()
            .args(["run", "--bundle"])
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
            readers.push(spawn_line_reader(
                stdout,
                slipfork_world::Stream::Stdout,
                sender.clone(),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_line_reader(
                stderr,
                slipfork_world::Stream::Stderr,
                sender.clone(),
            ));
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

    /// `crun kill --all` then `crun delete --force`; both are best-effort teardown.
    fn delete(&self, id: &str) {
        for args in [
            ["kill", "--all", id].as_slice(),
            ["delete", "--force", id].as_slice(),
        ] {
            let _ = self
                .shared
                .crun_command()
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Runs one already-written bundle to container start and returns its host pid.
fn crun_run(
    shared: &WorldShared,
    bundle: &Path,
    id: &str,
    sink: Arc<Mutex<LogSink>>,
) -> Result<i32, Fault> {
    let action = format!("pod `{id}` launch with crun");
    let pid_file = bundle.join("init.pid");
    let _ = fs::remove_file(&pid_file);
    let mut child = shared
        .crun_command()
        .args(["run", "--detach", "--bundle"])
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
        spawn_pump(stdout, Arc::clone(&sink));
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_pump(stderr, Arc::clone(&sink));
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

fn spawn_line_reader<R: Read + Send + 'static>(
    pipe: R,
    kind: slipfork_world::Stream,
    sender: mpsc::Sender<(slipfork_world::Stream, String)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let _ = sender.send((kind, line));
        }
    })
}

fn spawn_pump<R: Read + Send + 'static>(mut reader: R, sink: Arc<Mutex<LogSink>>) {
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
