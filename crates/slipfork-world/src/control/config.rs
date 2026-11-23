//! Typed request/reply clients over `<pods>/control.sock`: `ConfigureS3`, `List`, and `RunOnce`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::{
    FrameLines, InitFrame, PROTOCOL_VERSION, Request, S3WorldConfig, Stream, control_socket_path,
    decode_init_frame, encode,
};
use crate::error::{Error, Result};
use crate::pod::PodSpec;
use crate::supervisor::{PodStatus, pod_statuses};

/// Binds the world's native S3 server and returns its typed reply payload.
///
/// # Errors
///
/// Fails closed when the world is gone, the frame is malformed, or the init refuses the bind.
pub fn configure_s3(pods_root: &Path, config: S3WorldConfig) -> Result<serde_json::Value> {
    let id = format!("s3-{}", config.listen_addr.port());
    let request = Request::ConfigureS3 {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        config,
    };
    call_once(pods_root, &id, &request, Duration::from_secs(10))
}

/// Queries the world's tracked pods over `<pods>/control.sock` with no supervisor lock held.
pub fn list_pods(pods_root: &Path) -> Result<Vec<PodStatus>> {
    let id = "list";
    let request = Request::List {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
    };
    let reply = call_once(pods_root, id, &request, Duration::from_secs(5))?;
    pod_statuses(&reply)
}

/// Launches one already-bundled detached pod over `<pods>/control.sock` concurrently, so sibling
/// `crun run --detach` spawns and `Dial` readiness probes never serialize on one pipe.
pub fn launch_pod(pods_root: &Path, pod: &PodSpec, timeout: Duration) -> Result<u32> {
    let id = format!("launch-{}", pod.id);
    let request = Request::Launch {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        pod: pod.clone(),
    };
    let ok = call_once(pods_root, &id, &request, timeout).map_err(|error| match error {
        Error::Protocol { message } => Error::PodLaunch { message },
        other => other,
    })?;
    ok.get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| Error::Protocol {
            message: "launch reply carried no pid".to_owned(),
        })
}

/// Runs one already-bundled setup/hook pod to completion over `<pods>/control.sock`, streaming
/// every stdout/stderr line live without holding the world supervisor channel.
pub fn run_once(
    pods_root: &Path,
    pod: &PodSpec,
    timeout: Duration,
    on_line: &mut dyn FnMut(Stream, &str),
) -> Result<i32> {
    let path = control_socket_path(pods_root)?;
    let mut socket =
        UnixStream::connect(&path).map_err(|source| Error::io("connect", &path, source))?;
    let wait_budget = timeout.saturating_add(Duration::from_secs(5));
    socket
        .set_read_timeout(Some(wait_budget))
        .and_then(|()| socket.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|source| Error::io("configure", &path, source))?;
    let id = format!("run-{}", pod.id);
    let request = Request::RunOnce {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        pod: pod.clone(),
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
    };
    socket
        .write_all(&encode(&request)?)
        .and_then(|()| socket.flush())
        .map_err(|source| Error::io("write", &path, source))?;
    let mut lines = FrameLines::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(raw) = lines.next_line() {
            let text = std::str::from_utf8(&raw).map_err(|source| Error::Protocol {
                message: format!("control frame is not UTF-8: {source}"),
            })?;
            match decode_init_frame(text)? {
                InitFrame::Output {
                    id: out_id,
                    stream,
                    line,
                    ..
                } if out_id == id => on_line(stream, &line),
                InitFrame::Reply {
                    id: reply_id,
                    ok,
                    error,
                    ..
                } if reply_id == id => {
                    if let Some(error) = error {
                        return Err(Error::PodLaunch {
                            message: if error.message.is_empty() {
                                error.code
                            } else {
                                error.message
                            },
                        });
                    }
                    return ok
                        .and_then(|ok| ok.get("exit_code").and_then(serde_json::Value::as_i64))
                        .and_then(|code| i32::try_from(code).ok())
                        .ok_or_else(|| Error::Protocol {
                            message: "run-once reply carried no exit code".to_owned(),
                        });
                }
                InitFrame::Stopped { .. } => {
                    return Err(Error::Init {
                        message: "world init stopped while a run-once was pending".to_owned(),
                    });
                }
                InitFrame::Ready { .. } | InitFrame::Reply { .. } | InitFrame::Output { .. } => {}
            }
            continue;
        }
        let read = match socket.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(Error::io("read", &path, source)),
        };
        if read == 0 {
            return Err(Error::Init {
                message: "world init closed its control socket".to_owned(),
            });
        }
        lines.extend(&chunk[..read]);
    }
}

fn call_once(
    pods_root: &Path,
    id: &str,
    request: &Request,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let path = control_socket_path(pods_root)?;
    let mut socket =
        UnixStream::connect(&path).map_err(|source| Error::io("connect", &path, source))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|source| Error::io("configure", &path, source))?;
    socket
        .write_all(&encode(request)?)
        .and_then(|()| socket.flush())
        .map_err(|source| Error::io("write", &path, source))?;
    let mut lines = FrameLines::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(raw) = lines.next_line() {
            let text = std::str::from_utf8(&raw).map_err(|source| Error::Protocol {
                message: format!("control frame is not UTF-8: {source}"),
            })?;
            match decode_init_frame(text)? {
                InitFrame::Reply {
                    id: reply_id,
                    ok,
                    error,
                    ..
                } if reply_id == id => {
                    return super::stream::finish(ok, error);
                }
                InitFrame::Ready { .. } | InitFrame::Output { .. } => continue,
                InitFrame::Stopped { .. } => {
                    return Err(Error::Init {
                        message: "the world stopped while handling a control request".to_owned(),
                    });
                }
                InitFrame::Reply { .. } => continue,
            }
        }
        let read = match socket.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(Error::io("read", &path, source)),
        };
        if read == 0 {
            return Err(Error::Protocol {
                message: "the world closed its control socket".to_owned(),
            });
        }
        lines.extend(&chunk[..read]);
    }
}
