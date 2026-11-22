//! Pod launch and run-to-completion: `crun run`, log pumps, and abort-safe teardown.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;
use slipfork_world::control::FrameLines;
use slipfork_world::pod_log::{CHUNK, LogKind, LogSink};

use crate::shared::{PodInfo, WorldShared, exit_code, reap_zombies, wait_child};
use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

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
pub(super) fn crun_delete(shared: &WorldShared, id: &str) {
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
pub(super) fn crun_run(
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
