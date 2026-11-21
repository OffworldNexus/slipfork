//! Streaming control-socket requests: timestamp-merged `logs` replay and interactive `exec`.
//!
//! Both handlers run on a detached thread so the init loop keeps reaping pods and answering the
//! lifeline. They never touch `InitWorld` directly: [`WorldShared`] exposes only cloned facts.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use slipfork_world::control::{
    self, ControlError, FrameLines, InitFrame, PROTOCOL_VERSION, Request, Stream,
};
use slipfork_world::pod_log::{LogLine, LogMerge, LogReader, REORDER_MS, now_ms};

use crate::pod::{WorldShared, reap_zombies};
use crate::{Fault, REMEDY_CRUN, REMEDY_PROTOCOL};

/// How long an idle follower sleeps before checking its stop conditions again.
const IDLE: Duration = Duration::from_millis(80);

/// Replays (and optionally follows) `<pods>/logs/<id>.log` into `stream`, merged by timestamp.
pub(crate) fn serve_logs(
    shared: Arc<WorldShared>,
    request_id: String,
    service: Option<String>,
    follow: bool,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let mut ids = shared.ids();
    ids.sort();
    if let Some(service) = &service {
        if !ids.iter().any(|id| id == service) {
            return refuse(
                &mut stream,
                &request_id,
                "no-pod",
                format!("pod `{service}` is not running"),
            );
        }
        ids = vec![service.clone()];
    }
    let mut readers: Vec<(String, LogReader)> = Vec::new();
    for id in &ids {
        let Some(info) = shared.info(id) else {
            continue;
        };
        if let Ok(reader) = LogReader::open(&info.log) {
            readers.push((id.clone(), reader));
        }
    }
    let mut merge = LogMerge::default();
    let mut closed = false;
    loop {
        let mut progress = false;
        for (id, reader) in &mut readers {
            let mut route = |line: LogLine| merge.push(id, line);
            match reader.read_lines(&mut route) {
                Ok(read) => progress |= read > 0,
                Err(source) => {
                    let _ = Fault::cause(
                        format!("replay pod `{id}` log"),
                        crate::REMEDY_RUNTIME,
                        source,
                    );
                    closed = true;
                    break;
                }
            }
        }
        let batch = if follow {
            merge.drain(now_ms().saturating_sub(REORDER_MS))
        } else {
            merge.flush()
        };
        for (pod, line) in batch {
            write_output(
                &mut stream,
                &request_id,
                &pod,
                Stream::Combined,
                output_kind(line.kind),
                &line.text,
            )?;
        }
        if closed || !follow || shared.stopped() || peer_closed(&stream) {
            break;
        }
        if !progress {
            thread::sleep(IDLE);
        }
    }
    for (pod, line) in merge.flush() {
        write_output(
            &mut stream,
            &request_id,
            &pod,
            Stream::Combined,
            output_kind(line.kind),
            &line.text,
        )?;
    }
    for id in &ids {
        write_output(
            &mut stream,
            &request_id,
            id,
            Stream::Combined,
            slipfork_world::OutputKind::Eof,
            "",
        )?;
    }
    reply(
        &mut stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id,
            ok: Some(serde_json::json!({ "pods": ids, "follow": follow })),
            error: None,
        },
    )
}

/// Runs one command inside a live pod through `crun exec` and streams its stdio.
pub(crate) fn serve_exec(
    shared: Arc<WorldShared>,
    request_id: String,
    pod: String,
    argv: Vec<String>,
    stdin: bool,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let Some(info) = shared.info(&pod) else {
        return refuse(
            &mut stream,
            &request_id,
            "no-pod",
            format!("pod `{pod}` is not running"),
        );
    };
    if argv.is_empty() {
        return refuse(
            &mut stream,
            &request_id,
            "empty-argv",
            "no command given".into(),
        );
    }
    let mut command = Command::new(shared.crun());
    command
        .args(["--cgroup-manager=disabled", "exec", "--cwd"])
        .arg(&info.workdir)
        .arg(&pod)
        .args(&argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .process_group(0);
    let mut child = command.spawn().map_err(|source| {
        Fault::cause(
            format!("exec in pod `{pod}` with crun"),
            REMEDY_CRUN,
            source,
        )
    })?;
    let child_pid = i32::try_from(child.id()).unwrap_or_default();
    shared.register_exec(child_pid);
    let client_gone = Arc::new(AtomicBool::new(false));
    let slot = child
        .stdin
        .take()
        .map(|stdin| Arc::new(Mutex::new(Some(stdin))));
    let input_thread = match slot.clone() {
        Some(slot) => {
            let input = stream
                .try_clone()
                .map_err(|source| Fault::cause("exec input clone", REMEDY_PROTOCOL, source))?;
            let gone = Arc::clone(&client_gone);
            Some(thread::spawn(move || pump_input(input, slot, gone)))
        }
        None => None,
    };

    let (sender, receiver) = mpsc::channel::<(Stream, String)>();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let sender = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send((Stream::Stdout, line));
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let sender = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send((Stream::Stderr, line));
            }
        }));
    }
    drop(sender);

    let mut deliver = true;
    loop {
        match receiver.recv_timeout(IDLE) {
            Ok((kind, line)) => {
                if write_output(
                    &mut stream,
                    &request_id,
                    &pod,
                    kind,
                    slipfork_world::OutputKind::Line,
                    &line,
                )
                .is_err()
                {
                    deliver = false;
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.stopped() || client_gone.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if (!deliver || client_gone.load(Ordering::Relaxed) || shared.stopped()) && child_pid != 0 {
        slipfork_world::kill_group(child_pid);
    }
    let code = wait_child(&mut child, &shared, child_pid);
    shared.unregister_exec(child_pid);
    // Unblock the stdin pump (it blocks on this socket, not on the child) and its readers.
    let _ = stream.shutdown(Shutdown::Read);
    if let Some(handle) = input_thread {
        let _ = handle.join();
    }
    for reader in readers {
        let _ = reader.join();
    }
    reap_zombies(&shared);
    if !deliver {
        return Ok(());
    }
    reply(
        &mut stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id,
            ok: Some(serde_json::json!({ "exit_code": code, "pod": pod })),
            error: None,
        },
    )
}

/// Reads `ExecInput` frames from a cloned connection and feeds the child's stdin.
///
/// A socket EOF (the daemon vanished) marks `gone` so the exec child is killed instead of hanging
/// the init. An explicit empty input frame is a normal stdin close and leaves the child running.
fn pump_input(mut stream: UnixStream, slot: Arc<Mutex<Option<ChildStdin>>>, gone: Arc<AtomicBool>) {
    let mut bytes = [0_u8; 4096];
    let mut lines = FrameLines::new();
    loop {
        let read = match stream.read(&mut bytes) {
            Ok(0) => {
                gone.store(true, Ordering::Relaxed);
                close_stdin(&slot);
                return;
            }
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                gone.store(true, Ordering::Relaxed);
                close_stdin(&slot);
                return;
            }
        };
        for raw in lines.feed(&bytes[..read], slipfork_world::pod_log::CHUNK) {
            let Ok(text) = std::str::from_utf8(&raw) else {
                continue;
            };
            let Ok(Request::ExecInput { data, .. }) = control::decode_request(text.trim()) else {
                continue;
            };
            if data.is_empty() || data == "EOF" {
                // An explicit empty frame is a normal stdin close; keep watching for a socket EOF
                // so a vanished daemon still kills an exec child that ignores EOF.
                close_stdin(&slot);
                continue;
            }
            let Ok(payload) = base64::engine::general_purpose::STANDARD.decode(data) else {
                continue;
            };
            if let Ok(mut guard) = slot.lock()
                && let Some(stdin) = guard.as_mut()
            {
                let _ = stdin.write_all(&payload);
                let _ = stdin.flush();
            }
        }
    }
}

fn close_stdin(slot: &Arc<Mutex<Option<ChildStdin>>>) {
    if let Ok(mut guard) = slot.lock() {
        guard.take();
    }
}

/// Waits for the exec child, recovering a status the init loop may have reaped first.
fn wait_child(child: &mut Child, shared: &WorldShared, pid: i32) -> i32 {
    match child.wait() {
        Ok(status) => status.code().unwrap_or(128),
        Err(_) => {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if let Some(code) = shared.take_status(pid) {
                    return code;
                }
                thread::sleep(Duration::from_millis(10));
            }
            -1
        }
    }
}

/// True once the client closed its end of the streaming connection.
fn peer_closed(stream: &UnixStream) -> bool {
    let mut pollfd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call; zero timeout never blocks.
    let ret = unsafe { libc::poll(&mut pollfd, 1, 0) };
    ret > 0
        && pollfd.revents & (libc::POLLHUP | libc::POLLRDHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

fn write_output(
    stream: &mut UnixStream,
    request_id: &str,
    pod: &str,
    stream_kind: Stream,
    kind: slipfork_world::OutputKind,
    line: &str,
) -> Result<(), Fault> {
    reply(
        stream,
        &InitFrame::Output {
            v: PROTOCOL_VERSION,
            id: request_id.to_owned(),
            pod: Some(pod.to_owned()),
            stream: stream_kind,
            kind,
            line: line.to_owned(),
        },
    )
}

fn refuse(
    stream: &mut UnixStream,
    request_id: &str,
    code: &str,
    message: String,
) -> Result<(), Fault> {
    reply(
        stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id.to_owned(),
            ok: None,
            error: Some(ControlError {
                code: code.to_owned(),
                message,
            }),
        },
    )
}

fn reply(stream: &mut UnixStream, frame: &InitFrame) -> Result<(), Fault> {
    let bytes = control::encode(frame)
        .map_err(|source| Fault::cause("stream frame encode", REMEDY_PROTOCOL, source))?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| Fault::cause("stream frame write", REMEDY_PROTOCOL, source))
}

#[allow(dead_code)]
fn base64_decode(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Maps a capture line kind onto its wire frame kind.
fn output_kind(kind: slipfork_world::pod_log::LogKind) -> slipfork_world::OutputKind {
    match kind {
        slipfork_world::pod_log::LogKind::Line => slipfork_world::OutputKind::Line,
        slipfork_world::pod_log::LogKind::Divider => slipfork_world::OutputKind::Divider,
    }
}
