//! Pod launch and run-to-completion: `crun run`, log pumps, and abort-safe teardown.

use std::fs;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::control::FrameLines;
use slipfork_world::pod_log::{CHUNK, LogSink, now_ms};
use slipfork_world::{PodSpec, StoreLayout};

use crate::shared::{LaunchClaim, PodInfo, WorldShared, exit_code, reap_zombies, wait_child};
use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

/// Launches one detached pod concurrently against `WorldShared` without holding a lock across
/// `crun run --detach`.
pub(crate) fn launch_on_shared(shared: &WorldShared, pod: &PodSpec) -> Result<i32, Fault> {
    let action = format!("pod `{}` launch with crun", pod.id);
    pod.validate()
        .map_err(|source| Fault::cause(action.clone(), REMEDY_POD, source))?;
    // Concurrent `Launch` frames for one id must share one `crun run`: the loser waits for the
    // winner and reuses its pid instead of failing with `container already exists`.
    let _slot = match shared.claim_launch(&pod.id)? {
        LaunchClaim::Ready(pid) => return Ok(pid),
        LaunchClaim::Lead(slot) => slot,
    };
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
    // `StoreLayout` names the canonical `<store>/logs/<world>` root once. A rootfs outside
    // `<store>/pods/<world>/<slot>` is refused so no second naming path can invent a tmpfs capture.
    let logs_dir = match StoreLayout::from_pod_rootfs(&pod.rootfs) {
        Some((layout, world)) => layout.world_logs(&world),
        None => {
            return Err(Fault::new(
                format!(
                    "pod `{}` rootfs `{}` is not a store pod clone",
                    pod.id,
                    pod.rootfs.display()
                ),
                "launch pods from a `StoreLayout::world_pods` clone",
            ));
        }
    };
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
    let _ = shared.take_status(pid);
    shared.set_logs_root(logs_dir);
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
    // One reader owns both pipes: two threads racing separate pipes reorder a single child's
    // stdout/stderr (the PTY regression), so the merged reader drains them deterministically.
    let mut readers = vec![spawn_merged_reader(
        child.stdout.take(),
        child.stderr.take(),
        sender,
    )];
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
    let deadline = Instant::now() + Duration::from_secs(10);
    for args in [
        ["kill", "--all", id, "SIGKILL"].as_slice(),
        ["delete", "--force", id].as_slice(),
    ] {
        let _ = crun_in(shared, args, deadline);
    }
}

/// Runs one `crun` subcommand under a shared deadline.
///
/// A wedged `crun` is killed and reaped at the deadline, so one slow pod can never stretch the
/// whole world's teardown past its budget; the failure names the subcommand instead of vanishing.
pub(super) fn crun_in(
    shared: &WorldShared,
    args: &[&str],
    deadline: Instant,
) -> Result<(), String> {
    let action = args.first().copied().unwrap_or("run");
    let mut child = shared
        .crun_command()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| format!("`crun {action}` could not start: {source}"))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("`crun {action}` exited with {status}")),
            Ok(None) => {}
            Err(source) => return Err(format!("`crun {action}` wait: {source}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("`crun {action}` exceeded the teardown deadline"));
        }
        thread::sleep(Duration::from_millis(5));
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
    // One reader polls both pipes on a single thread and drains stdout before stderr when both are
    // ready simultaneously, matching `run_once_on_shared` so sequential `echo` order stays intact.
    let _ = spawn_merged_pump(
        child.stdout.take(),
        child.stderr.take(),
        move |stream, raw| append(&sink, raw, stream),
    );
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

/// Reads both child pipes from one thread, in one deterministic order.
///
/// Two byte streams on separate pipes carry no global order, and two reader threads make the
/// interleaving depend on scheduler luck. Polling both descriptors in one thread and draining
/// stdout first when both are ready keeps a sequential child's `echo` order intact.
fn spawn_merged_reader(
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    sender: mpsc::Sender<(slipfork_world::Stream, String)>,
) -> thread::JoinHandle<()> {
    spawn_merged_pump(stdout, stderr, move |stream, raw| {
        let text = String::from_utf8_lossy(raw)
            .trim_end_matches('\r')
            .to_owned();
        let _ = sender.send((stream, text));
    })
}

fn spawn_merged_pump<O, E, F>(
    stdout: Option<O>,
    stderr: Option<E>,
    on_line: F,
) -> thread::JoinHandle<()>
where
    O: Read + std::os::fd::AsRawFd + Send + 'static,
    E: Read + std::os::fd::AsRawFd + Send + 'static,
    F: FnMut(slipfork_world::Stream, &[u8]) + Send + 'static,
{
    thread::spawn(move || merged_pump(stdout, stderr, on_line))
}

fn merged_pump<O, E, F>(mut stdout: Option<O>, mut stderr: Option<E>, mut on_line: F)
where
    O: Read + std::os::fd::AsRawFd,
    E: Read + std::os::fd::AsRawFd,
    F: FnMut(slipfork_world::Stream, &[u8]),
{
    use std::os::fd::RawFd;

    fn poll_fd(fd: Option<RawFd>) -> libc::pollfd {
        libc::pollfd {
            fd: fd.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        }
    }

    fn has_unread_bytes(fd: RawFd) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` with zero timeout probes buffered bytes without blocking.
        unsafe { libc::poll(&mut pfd, 1, 0) > 0 && (pfd.revents & libc::POLLIN) != 0 }
    }

    let mut out_lines = FrameLines::new();
    let mut err_lines = FrameLines::new();
    loop {
        if stdout.is_none() && stderr.is_none() {
            break;
        }
        let mut fds = [
            poll_fd(stdout.as_ref().map(std::os::fd::AsRawFd::as_raw_fd)),
            poll_fd(stderr.as_ref().map(std::os::fd::AsRawFd::as_raw_fd)),
        ];
        // SAFETY: both descriptors in `fds` are valid for the call, and the 100ms bound keeps
        // this thread responsive to teardown while the child stays quiet.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if ready == 0 {
            continue;
        }
        if fds[0].revents != 0 {
            for _ in 0..8 {
                pump_pipe(
                    &mut stdout,
                    &mut out_lines,
                    slipfork_world::Stream::Stdout,
                    &mut on_line,
                );
                if stdout
                    .as_ref()
                    .is_none_or(|pipe| !has_unread_bytes(pipe.as_raw_fd()))
                {
                    break;
                }
            }
        }
        if fds[1].revents != 0 {
            pump_pipe(
                &mut stderr,
                &mut err_lines,
                slipfork_world::Stream::Stderr,
                &mut on_line,
            );
        }
    }
}

fn pump_pipe<R, F>(
    pipe: &mut Option<R>,
    lines: &mut FrameLines,
    stream: slipfork_world::Stream,
    on_line: &mut F,
) where
    R: Read,
    F: FnMut(slipfork_world::Stream, &[u8]),
{
    let Some(reader) = pipe.as_mut() else {
        return;
    };
    let mut chunk = [0_u8; 8192];
    match reader.read(&mut chunk) {
        Ok(0) => {
            *pipe = None;
            if let Some(raw) = lines.take_partial() {
                on_line(stream, &raw);
            }
        }
        Ok(read) => {
            for raw in lines.feed(&chunk[..read], CHUNK) {
                on_line(stream, &raw);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        Err(_) => *pipe = None,
    }
}

fn append(sink: &Mutex<LogSink>, raw: &[u8], stream: slipfork_world::Stream) {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches('\r');
    if let Ok(mut sink) = sink.lock() {
        let _ = sink.append_stream(now_ms(), stream, text);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::merged_pump;
    use slipfork_world::Stream;

    #[test]
    fn simultaneous_stdout_and_stderr_readiness_drains_stdout_before_stderr() {
        let (out_read, mut out_write) = std::io::pipe().expect("stdout pipe");
        let (err_read, mut err_write) = std::io::pipe().expect("stderr pipe");
        out_write
            .write_all(b"starting web\n")
            .expect("write stdout");
        err_write
            .write_all(b"boom: missing DATABASE_URL\n")
            .expect("write stderr");
        drop(out_write);
        drop(err_write);

        let mut seen = Vec::new();
        merged_pump(Some(out_read), Some(err_read), |stream, raw| {
            seen.push((stream, String::from_utf8_lossy(raw).into_owned()));
        });
        assert_eq!(
            seen,
            vec![
                (Stream::Stdout, "starting web".to_owned()),
                (Stream::Stderr, "boom: missing DATABASE_URL".to_owned()),
            ]
        );
    }
}
