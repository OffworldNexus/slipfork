//! Wire framing, the mode-`0600` exec process spec, and the stdin pump shared by `exec`/`logs`.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use slipfork_world::control::{
    self, ControlError, FrameLines, InitFrame, PROTOCOL_VERSION, Request, Stream,
};

use crate::pod::PodInfo;
use crate::{Fault, REMEDY_PROTOCOL, REMEDY_RUNTIME};

/// How long an idle follower sleeps before checking its stop conditions again.
pub(super) const IDLE: Duration = Duration::from_millis(80);

/// Mode-`0600` tmpfs OCI `process.json` derived from a pod's `config.json`, unlinked on drop.
pub(super) struct TempProcessSpec(PathBuf);

impl TempProcessSpec {
    pub(super) fn write(info: &PodInfo, seq: u64, argv: &[String]) -> Result<Self, Fault> {
        Self::write_with_terminal(info, seq, argv, false)
    }

    /// Builds the same identity/env-preserving spec with `"terminal": true` for `ExecPty`.
    pub(super) fn write_pty(info: &PodInfo, seq: u64, argv: &[String]) -> Result<Self, Fault> {
        Self::write_with_terminal(info, seq, argv, true)
    }

    fn write_with_terminal(
        info: &PodInfo,
        seq: u64,
        argv: &[String],
        terminal: bool,
    ) -> Result<Self, Fault> {
        let config_path = info.bundle.join("config.json");
        let raw = fs::read(&config_path)
            .map_err(|source| Fault::cause("pod config read for exec", REMEDY_RUNTIME, source))?;
        let mut config: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|source| Fault::cause("pod config parse for exec", REMEDY_RUNTIME, source))?;
        let process = config
            .get_mut("process")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| Fault::new("pod config missing `process`", REMEDY_RUNTIME))?;
        process.insert("args".to_owned(), serde_json::json!(argv));
        process.insert("cwd".to_owned(), serde_json::json!(info.workdir));
        process.insert("terminal".to_owned(), serde_json::Value::Bool(terminal));
        let rendered = serde_json::to_vec(&serde_json::Value::Object(std::mem::take(process)))
            .map_err(|source| Fault::cause("exec process encode", REMEDY_RUNTIME, source))?;
        let path = info.bundle.join(format!("exec-{seq}.json"));
        let _ = fs::remove_file(&path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| Fault::cause("exec process spec create", REMEDY_RUNTIME, source))?;
        let guard = Self(path);
        file.write_all(&rendered)
            .map_err(|source| Fault::cause("exec process spec write", REMEDY_RUNTIME, source))?;
        Ok(guard)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempProcessSpec {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// True once the client closed its end of the streaming connection.
pub(super) fn peer_closed(stream: &UnixStream) -> bool {
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

pub(super) fn write_output(
    stream: &mut UnixStream,
    request_id: &str,
    pod: &str,
    stream_kind: Stream,
    kind: slipfork_world::OutputKind,
    ts_ms: Option<u64>,
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
            ts_ms,
            line: line.to_owned(),
        },
    )
}

pub(super) fn refuse(
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

pub(super) fn reply(stream: &mut UnixStream, frame: &InitFrame) -> Result<(), Fault> {
    let bytes = control::encode(frame)
        .map_err(|source| Fault::cause("stream frame encode", REMEDY_PROTOCOL, source))?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| Fault::cause("stream frame write", REMEDY_PROTOCOL, source))
}

/// Maps a capture line kind onto its wire frame kind.
pub(super) fn output_kind(kind: slipfork_world::pod_log::LogKind) -> slipfork_world::OutputKind {
    match kind {
        slipfork_world::pod_log::LogKind::Line => slipfork_world::OutputKind::Line,
        // Build instruction rows only ever live in an image/run capture; they never cross a pod
        // stream, so they map onto the divider frame rather than growing a new wire variant.
        slipfork_world::pod_log::LogKind::Step => slipfork_world::OutputKind::Divider,
        slipfork_world::pod_log::LogKind::Divider => slipfork_world::OutputKind::Divider,
    }
}

/// Reads `ExecInput` frames from a cloned connection and feeds the child's stdin.
///
/// Any frames already buffered alongside the opening `Exec` request are drained first. A socket
/// EOF (the daemon vanished) marks `gone` so the exec child is killed instead of hanging the init;
/// an explicit empty input frame is a normal stdin close and leaves the child running.
pub(super) fn pump_input(
    mut stream: UnixStream,
    mut lines: FrameLines,
    slot: Arc<Mutex<Option<ChildStdin>>>,
    gone: Arc<AtomicBool>,
) {
    while let Some(raw) = lines.next_line() {
        forward_input(&raw, &slot);
    }
    let mut bytes = [0_u8; 4096];
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
            forward_input(&raw, &slot);
        }
    }
}

fn forward_input(raw: &[u8], slot: &Arc<Mutex<Option<ChildStdin>>>) {
    let Ok(text) = std::str::from_utf8(raw) else {
        return;
    };
    let Ok(Request::ExecInput { data, .. }) = control::decode_request(text.trim()) else {
        return;
    };
    if data.is_empty() || data == "EOF" {
        close_stdin(slot);
        return;
    }
    let Ok(payload) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return;
    };
    if let Ok(mut guard) = slot.lock()
        && let Some(stdin) = guard.as_mut()
    {
        let _ = stdin.write_all(&payload);
        let _ = stdin.flush();
    }
}

pub(super) fn close_stdin(slot: &Arc<Mutex<Option<ChildStdin>>>) {
    if let Ok(mut guard) = slot.lock() {
        guard.take();
    }
}
