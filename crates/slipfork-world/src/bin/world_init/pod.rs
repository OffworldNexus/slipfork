//! Launched containers inside the world: `crun run`, captured logs, waits, restart, teardown.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;
use slipfork_world::control::{FrameLines, S3WorldConfig};
use slipfork_world::pod_log::{CHUNK, LogKind, LogSink};
use tokio::runtime::Handle;

use crate::s3::S3Host;
pub(crate) use crate::shared::{PodInfo, WorldShared, exit_code, reap_zombies, wait_child};
use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

pub(crate) struct InitWorld {
    shared: Arc<WorldShared>,
    /// The one runtime the native S3 endpoint is served on; never constructed in library code.
    runtime: Handle,
    s3: Option<S3Host>,
}

impl InitWorld {
    pub(crate) fn new(crun: PathBuf, pods_root: PathBuf, runtime: Handle) -> Result<Self, Fault> {
        let crun_root = pods_root.join("crun");
        fs::create_dir_all(&crun_root)
            .and_then(|()| fs::set_permissions(&crun_root, fs::Permissions::from_mode(0o700)))
            .map_err(|source| Fault::cause("crun state root setup", REMEDY_RUNTIME, source))?;
        Ok(Self {
            shared: Arc::new(WorldShared::new(crun, crun_root, pods_root)),
            runtime,
            s3: None,
        })
    }

    pub(crate) fn shared(&self) -> Arc<WorldShared> {
        Arc::clone(&self.shared)
    }

    pub(crate) fn pods_root(&self) -> &Path {
        self.shared.pods_root()
    }

    /// Every tracked pod with its pid, reaping status, and exit code.
    pub(crate) fn list(&self) -> Vec<serde_json::Value> {
        list_on_shared(&self.shared)
    }

    /// Bounded wait: `Some(code)` when the pod exited, `None` while it is still running.
    pub(crate) fn wait(&self, pid: i32, timeout_ms: u64) -> Result<Option<i32>, Fault> {
        let tracked = self
            .shared
            .pids
            .lock()
            .map(|pids| pids.values().any(|candidate| *candidate == pid))
            .unwrap_or(false);
        if !tracked {
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
        launch_on_shared(&self.shared, pod)
    }

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
            slipfork_world::kill_group(pid);
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

    /// Binds and serves the declared buckets on the injected runtime, replacing any previous
    /// endpoint. Returns the bound address and the bucket count.
    pub(crate) fn configure_s3(
        &mut self,
        config: S3WorldConfig,
    ) -> Result<(SocketAddr, usize), Fault> {
        self.stop_s3();
        let host = S3Host::bind(&self.runtime, config)?;
        let reply = (host.listen_addr(), host.buckets());
        self.s3 = Some(host);
        Ok(reply)
    }

    fn stop_s3(&mut self) {
        if let Some(host) = self.s3.take() {
            host.shutdown();
        }
    }

    pub(crate) fn stop_all(&mut self) {
        self.stop_s3();
        self.shared.mark_stopped();
        for pid in self.shared.exec_pids() {
            slipfork_world::kill_group(pid);
        }
        let pids = self
            .shared
            .pids
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for (id, pid) in pids {
            crun_delete(&self.shared, &id);
            slipfork_world::kill_group(pid);
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(container) = pods.remove(&id)
            {
                let _ = fs::remove_dir_all(&container.bundle);
            }
        }
        if let Ok(mut logs) = self.shared.logs.lock() {
            logs.clear();
        }
        reap_zombies(&self.shared);
    }

    /// Runs one bundle to completion while forwarding every stdout/stderr line.
    pub(crate) fn run_once_streaming(
        &self,
        pod: &PodSpec,
        timeout_ms: u64,
        on_line: &mut dyn FnMut(slipfork_world::Stream, &str) -> Result<(), Fault>,
    ) -> Result<i32, Fault> {
        run_once_on_shared(&self.shared, pod, timeout_ms, on_line)
    }
}

/// Launches one detached pod concurrently against `WorldShared` without holding a lock across
/// `crun run --detach`.
pub(crate) fn launch_on_shared(shared: &WorldShared, pod: &PodSpec) -> Result<i32, Fault> {
    let action = format!("pod `{}` launch with crun", pod.id);
    pod.validate()
        .map_err(|source| Fault::cause(action.clone(), REMEDY_POD, source))?;
    if shared
        .pids
        .lock()
        .is_ok_and(|pids| pids.contains_key(&pod.id))
    {
        return Err(Fault::new(
            format!("pod `{}` is already running", pod.id),
            REMEDY_POD,
        ));
    }
    let bundle = canonical(
        &shared.pods_root().join(&pod.id),
        action.clone(),
        REMEDY_RUNTIME,
    )?;
    if !bundle.starts_with(shared.pods_root()) {
        return Err(Fault::new(
            action,
            "keep the pod runtime root on a tmpfs owned by this user",
        ));
    }
    let logs_dir = shared.pods_root().join("logs");
    let sink = LogSink::open(&logs_dir, &pod.id)
        .map_err(|source| Fault::cause(action.clone(), REMEDY_RUNTIME, source))?;
    let restarted = shared
        .logs
        .lock()
        .is_ok_and(|logs| logs.contains_key(&pod.id));
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
    let pid = crun_run(shared, &bundle, &pod.id, Arc::clone(&sink))?;
    if let Ok(mut logs) = shared.logs.lock() {
        logs.insert(pod.id.clone(), sink);
    }
    if let Ok(mut pids) = shared.pids.lock() {
        pids.insert(pod.id.clone(), pid);
    }
    if let Ok(mut pods) = shared.pods.lock() {
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

pub(crate) fn list_on_shared(shared: &WorldShared) -> Vec<serde_json::Value> {
    let entries: Vec<(String, i32)> = shared
        .pids
        .lock()
        .map(|pids| pids.iter().map(|(id, pid)| (id.clone(), *pid)).collect())
        .unwrap_or_default();
    entries
        .into_iter()
        .map(|(id, pid)| {
            let mut status = 0;
            // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid {
                shared.record_status(pid, exit_code(status));
            }
            serde_json::json!({
                "id": id,
                "pid": pid,
                "running": reaped != pid
                    && shared.peek_status(pid).is_none()
                    && Path::new(&format!("/proc/{pid}")).exists(),
                "exit_code": shared.peek_status(pid),
            })
        })
        .collect()
}

/// Runs one bundle to completion against `WorldShared`, safe to invoke from detached socket
/// threads while PID 1 continues serving `Dial` and `Launch` requests.
pub(crate) fn run_once_on_shared(
    shared: &WorldShared,
    pod: &PodSpec,
    timeout_ms: u64,
    on_line: &mut dyn FnMut(slipfork_world::Stream, &str) -> Result<(), Fault>,
) -> Result<i32, Fault> {
    let bundle = shared.pods_root().join(&pod.id);
    if !bundle.starts_with(shared.pods_root()) {
        return Err(Fault::new(
            format!("pod `{}` bundle escapes the runtime root", pod.id),
            REMEDY_CRUN,
        ));
    }
    let action = format!("pod `{}` run-once with crun", pod.id);
    let mut child = shared
        .crun_command()
        .args(["run", "--bundle"])
        .arg(&bundle)
        .arg(&pod.id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))?;
    let child_pid = i32::try_from(child.id()).unwrap_or_default();
    shared.register_exec(child_pid);
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
            abort_run_once(shared, &pod.id, child_pid, &mut child, &mut readers);
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
                    abort_run_once(shared, &pod.id, child_pid, &mut child, &mut readers);
                    return Err(fault);
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    for (reader, kind) in readers.into_iter().zip(["stdout", "stderr"]) {
        if reader.join().is_err() {
            let _ = wait_child(&mut child, shared, child_pid);
            shared.unregister_exec(child_pid);
            crun_delete(shared, &pod.id);
            reap_zombies(shared);
            return Err(Fault::new(
                format!("pod `{}` {kind} reader panicked", pod.id),
                REMEDY_CRUN,
            ));
        }
    }
    let code = wait_child(&mut child, shared, child_pid);
    shared.unregister_exec(child_pid);
    crun_delete(shared, &pod.id);
    reap_zombies(shared);
    Ok(code)
}

fn abort_run_once(
    shared: &WorldShared,
    id: &str,
    pid: i32,
    child: &mut std::process::Child,
    readers: &mut Vec<thread::JoinHandle<()>>,
) {
    if pid != 0 {
        slipfork_world::kill_group(pid);
    }
    crun_delete(shared, id);
    let _ = wait_child(child, shared, pid);
    shared.unregister_exec(pid);
    reap_zombies(shared);
    for reader in readers.drain(..) {
        let _ = reader.join();
    }
}

/// `crun kill --all SIGKILL` then `crun delete --force`; both are best-effort teardown.
fn crun_delete(shared: &WorldShared, id: &str) {
    for args in [
        ["kill", "--all", id, "SIGKILL"].as_slice(),
        ["delete", "--force", id].as_slice(),
    ] {
        let _ = shared
            .crun_command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
    let launcher_pid = i32::try_from(child.id()).unwrap_or_default();
    if let Some(stdout) = child.stdout.take() {
        spawn_pump(stdout, Arc::clone(&sink));
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_pump(stderr, Arc::clone(&sink));
    }
    let code = wait_child(&mut child, shared, launcher_pid);
    if code != 0 {
        return Err(Fault::new(
            format!("pod `{id}` launch with crun (`crun run` exited with {code})"),
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
