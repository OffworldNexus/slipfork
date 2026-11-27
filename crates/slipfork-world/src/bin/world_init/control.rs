//! The world's single control plane: `<pods>/control.sock`, mode `0600`, NDJSON `Request` frames.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use slipfork_world::PodSpec;
use slipfork_world::control::{self as wire, FrameLines, InitFrame, PROTOCOL_VERSION};

use crate::fault::failure_frame;
use crate::pod::{InitWorld, WorldShared, run_once_on_shared};
use crate::stream::{PtyRequest, serve_exec, serve_exec_pty, serve_logs};
use crate::{Fault, REMEDY_POD, REMEDY_PROTOCOL, REMEDY_RUNTIME, respond};

/// Longest a dial may block before the world reports a refused port.
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on a single NDJSON control request frame.
const FRAME_CAP: usize = 1 << 20;

/// Binds `<pods>/control.sock` mode `0600`, replacing a stale socket from a killed world.
pub(crate) fn bind(pods_root: &Path) -> Result<(UnixListener, PathBuf), Fault> {
    let path = wire::control_socket_path(pods_root)
        .map_err(|source| Fault::cause("control socket path check", REMEDY_RUNTIME, source))?;
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|source| {
        Fault::cause(
            format!("control socket bind at `{}`", path.display()),
            REMEDY_RUNTIME,
            source,
        )
    })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        Fault::cause(
            format!("control socket chmod at `{}`", path.display()),
            REMEDY_RUNTIME,
            source,
        )
    })?;
    listener.set_nonblocking(true).map_err(|source| {
        Fault::cause("control socket nonblocking mode", REMEDY_RUNTIME, source)
    })?;
    Ok((listener, path))
}

/// Drains every pending pathname-socket connection; returns `true` once `shutdown` is requested.
pub(crate) fn handle(listener: &UnixListener, world: &mut InitWorld) -> Result<bool, Fault> {
    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(source) => {
                return Err(Fault::cause(
                    "control socket accept",
                    REMEDY_PROTOCOL,
                    source,
                ));
            }
        };
        let Some((request, buffered)) = read_request(&mut stream)? else {
            continue;
        };
        // A dial reply is a status byte plus an optional descriptor, not an NDJSON `InitFrame`.
        match request {
            wire::Request::Dial { port, .. } => {
                dial(&mut stream, port)?;
            }
            wire::Request::Launch { id, pod, .. } => {
                // A launch of an already-live pod is ready, never an `already running` failure:
                // the daemon's reconcile re-applies a plan over surviving pods.
                if let Some(pid) = live_pid(world, &pod.id) {
                    reply(
                        &mut stream,
                        &InitFrame::Reply {
                            v: PROTOCOL_VERSION,
                            id,
                            ok: Some(serde_json::json!({ "pid": pid })),
                            error: None,
                        },
                    )?;
                    continue;
                }
                let shared = world.shared();
                std::thread::spawn(move || {
                    let frame = match crate::pod::launch_on_shared(&shared, &pod) {
                        Ok(pid) => InitFrame::Reply {
                            v: PROTOCOL_VERSION,
                            id,
                            ok: Some(serde_json::json!({ "pid": pid })),
                            error: None,
                        },
                        Err(fault) => {
                            eprintln!("world init: {fault}");
                            failure_frame(id, "launch-failed", &fault)
                        }
                    };
                    let _ = reply(&mut stream, &frame);
                });
            }
            wire::Request::Stop { id, pod, .. } => {
                let frame = match stop_pod(world, &pod) {
                    Ok(pid) => InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: Some(serde_json::json!({ "pid": pid, "stopped": true })),
                        error: None,
                    },
                    Err(fault) => {
                        eprintln!("world init: {fault}");
                        failure_frame(id, "stop-failed", &fault)
                    }
                };
                reply(&mut stream, &frame)?;
            }
            wire::Request::RunOnce {
                id,
                pod,
                timeout_ms,
                ..
            } => {
                let shared = world.shared();
                std::thread::spawn(move || {
                    if let Err(fault) = serve_run_once(&shared, id, pod, timeout_ms, stream) {
                        eprintln!("world init: {fault}");
                    }
                });
            }
            wire::Request::Logs {
                id,
                service,
                pods,
                follow,
                since_ms,
                until_ms,
                ..
            } => {
                let shared = world.shared();
                std::thread::spawn(move || {
                    if let Err(fault) = serve_logs(
                        shared, id, service, pods, follow, since_ms, until_ms, stream,
                    ) {
                        eprintln!("world init: {fault}");
                    }
                });
            }
            wire::Request::Exec {
                id,
                pod,
                argv,
                stdin,
                ..
            } => {
                let shared = world.shared();
                std::thread::spawn(move || {
                    if let Err(fault) = serve_exec(shared, id, pod, argv, stdin, stream, buffered) {
                        eprintln!("world init: {fault}");
                    }
                });
            }
            wire::Request::ExecPty {
                id,
                pod,
                argv,
                cols,
                rows,
                ..
            } => {
                let shared = world.shared();
                let request = PtyRequest {
                    id,
                    pod,
                    argv,
                    cols,
                    rows,
                    buffered,
                };
                std::thread::spawn(move || {
                    if let Err(fault) = serve_exec_pty(shared, request, stream) {
                        eprintln!("world init: {fault}");
                    }
                });
            }
            request => {
                let (frame, shutdown) =
                    respond(world, request, &mut |frame| reply(&mut stream, &frame))?;
                reply(&mut stream, &frame)?;
                if shutdown {
                    return Ok(true);
                }
            }
        }
    }
}

fn serve_run_once(
    shared: &WorldShared,
    id: String,
    pod: PodSpec,
    timeout_ms: u64,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let frame_id = id.clone();
    let pod_id = pod.id.clone();
    let outcome = run_once_on_shared(shared, &pod, timeout_ms, &mut |kind, line| {
        reply(
            &mut stream,
            &InitFrame::Output {
                v: PROTOCOL_VERSION,
                id: frame_id.clone(),
                pod: Some(pod_id.clone()),
                stream: kind,
                kind: slipfork_world::OutputKind::Line,
                ts_ms: None,
                line: line.to_owned(),
            },
        )
    });
    let final_frame = match outcome {
        Ok(code) => InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id,
            ok: Some(serde_json::json!({ "exit_code": code })),
            error: None,
        },
        Err(fault) => {
            eprintln!("world init: {fault}");
            let code = if fault.is_timeout() {
                "run-timeout"
            } else {
                "run-failed"
            };
            failure_frame(id, code, &fault)
        }
    };
    reply(&mut stream, &final_frame)
}

/// Reads the opening request frame while retaining any pipelined `ExecInput` bytes in `FrameLines`.
fn read_request(stream: &mut UnixStream) -> Result<Option<(wire::Request, FrameLines)>, Fault> {
    let mut lines = FrameLines::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(raw) = lines.next_line() {
            let text = std::str::from_utf8(&raw)
                .map_err(|source| Fault::cause("control socket utf-8", REMEDY_PROTOCOL, source))?
                .trim();
            if text.is_empty() {
                continue;
            }
            let request = wire::decode_request(text)
                .map_err(|source| Fault::cause("control socket decode", REMEDY_PROTOCOL, source))?;
            return Ok(Some((request, lines)));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read) => {
                lines.extend(&chunk[..read]);
                if lines.pending_len() > FRAME_CAP {
                    return Err(Fault::new("control frame exceeds 1 MiB", REMEDY_PROTOCOL));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Fault::cause("control socket read", REMEDY_PROTOCOL, source));
            }
        }
    }
}

/// Connects to `127.0.0.1:<port>` inside this world netns and hands the fd to the client.
///
/// Success is one `sendmsg` carrying payload `[0]` plus the descriptor; a refused or unreachable
/// port is payload `[1]` plus one JSON `ControlError`, so the daemon never invents an fd.
fn dial(stream: &mut UnixStream, port: u16) -> Result<(), Fault> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&address, DIAL_TIMEOUT) {
        Ok(upstream) => slipfork_world::fdpass::send_fd(stream, &[0], Some(upstream.as_fd()))
            .map_err(|source| Fault::cause("dial descriptor reply", REMEDY_PROTOCOL, source)),
        Err(source) => {
            let error = slipfork_world::ControlError {
                code: "dial-refused".to_owned(),
                message: format!("cannot connect to 127.0.0.1:{port}: {source}"),
            };
            let mut payload = vec![1_u8];
            payload.extend(
                wire::encode(&error)
                    .map_err(|error| Fault::cause("dial error encode", REMEDY_PROTOCOL, error))?,
            );
            slipfork_world::fdpass::send_fd(stream, &payload, None)
                .map_err(|source| Fault::cause("dial error reply", REMEDY_PROTOCOL, source))
        }
    }
}

/// The pid of a live tracked pod, when the plan re-applies over a surviving pod.
pub(crate) fn live_pid(world: &InitWorld, pod: &str) -> Option<i32> {
    world.list().into_iter().find_map(|status| {
        if status["id"] != pod || status["running"] != true {
            return None;
        }
        status["pid"]
            .as_i64()
            .and_then(|pid| i32::try_from(pid).ok())
    })
}

/// Stops one tracked pod: group kill, `crun delete`, bundle removal, and log-handle drop.
pub(crate) fn stop_pod(world: &mut InitWorld, pod: &str) -> Result<i32, Fault> {
    let shared = world.shared();
    let pid = shared
        .pids
        .lock()
        .map_err(|_| Fault::new(format!("pod `{pod}` pid lock"), REMEDY_RUNTIME))?
        .remove(pod);
    let Some(pid) = pid else {
        return Err(Fault::new(
            format!("pod `{pod}` is not running"),
            REMEDY_POD,
        ));
    };
    slipfork_world::kill_group(pid);
    for args in [
        ["kill", "--all", pod, "SIGKILL"].as_slice(),
        ["delete", "--force", pod].as_slice(),
    ] {
        let _ = shared
            .crun_command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if let Ok(mut pods) = shared.pods.lock()
        && let Some(info) = pods.remove(pod)
    {
        let _ = fs::remove_dir_all(&info.bundle);
    }
    if let Ok(mut logs) = shared.logs.lock() {
        logs.remove(pod);
    }
    Ok(pid)
}

fn reply(stream: &mut UnixStream, frame: &InitFrame) -> Result<(), Fault> {
    let bytes = wire::encode(frame)
        .map_err(|source| Fault::cause("control reply encode", REMEDY_PROTOCOL, source))?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| Fault::cause("control reply write", REMEDY_PROTOCOL, source))
}
