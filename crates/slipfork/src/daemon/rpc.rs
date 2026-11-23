//! Small world-control helpers: liveness probes and one typed request/reply call.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use slipfork_world::control::{self, InitFrame, Request};

/// Longest a reply may take; a real pod restart can exceed the old five seconds.
const REPLY_TIMEOUT: Duration = Duration::from_secs(300);

/// Stable registry key: the project path exactly as the client sent it.
pub(super) fn key_of(project: &Path) -> String {
    project.to_string_lossy().into_owned()
}

pub(super) fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

pub(super) fn world_answers(pods_dir: &Path) -> bool {
    control::control_socket_path(pods_dir)
        .ok()
        .is_some_and(|path| UnixStream::connect(path).is_ok())
}

/// Sends one request to `<pods>/control.sock` and returns its `ok` payload.
pub(super) fn world_call(pods_dir: &Path, request: &Request) -> Result<Value, String> {
    let path = control::control_socket_path(pods_dir).map_err(|error| error.to_string())?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("connect `{}`: {error}", path.display()))?;
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|error| format!("configure `{}`: {error}", path.display()))?;
    let bytes = control::encode(request).map_err(|error| error.to_string())?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|error| format!("send: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("read reply: {error}"))?;
    match control::decode_init_frame(line.trim()).map_err(|error| error.to_string())? {
        InitFrame::Reply { ok, error, .. } => match (error, ok) {
            (Some(error), _) => Err(error.message),
            (None, Some(value)) => Ok(value),
            (None, None) => Err("the world sent an empty reply".to_owned()),
        },
        InitFrame::Stopped { .. } => Err("the world stopped before replying".to_owned()),
        InitFrame::Output { .. } | InitFrame::Ready { .. } => {
            Err("unexpected frame on a reply-only channel".to_owned())
        }
    }
}
