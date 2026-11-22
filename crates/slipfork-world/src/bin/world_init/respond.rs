//! Request dispatch for the world init: launch, wait, list, restart, run-once, and refusals.

use slipfork_world::control::{InitFrame, PROTOCOL_VERSION, Request};

use crate::fault::{Fault, failure_frame, stopped_frame};
use crate::pod::InitWorld;

/// Builds the typed reply for one request; the boolean means "the world must exit".
///
/// `emit` writes intermediate frames (live [`InitFrame::Output`] lines) while a request runs.
/// Streaming `Logs`/`Exec` requests are served on the control socket instead.
pub(crate) fn respond(
    world: &mut InitWorld,
    request: Request,
    emit: &mut dyn FnMut(InitFrame) -> Result<(), Fault>,
) -> Result<(InitFrame, bool), Fault> {
    let (reply, shutdown) = match request {
        Request::Launch { id, pod, .. } => match world.launch(&pod) {
            Ok(pid) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "pid": pid })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "launch-failed", &fault), false)
            }
        },
        Request::StopAll { id, .. } => {
            world.stop_all();
            (stopped_frame(id), false)
        }
        Request::Shutdown { id, .. } => {
            world.stop_all();
            (stopped_frame(id), true)
        }
        Request::RunOnce {
            id,
            pod,
            timeout_ms,
            ..
        } => {
            let frame_id = id.clone();
            let pod_id = pod.id.clone();
            let mut on_line = |stream: slipfork_world::Stream, line: &str| {
                emit(InitFrame::Output {
                    v: PROTOCOL_VERSION,
                    id: frame_id.clone(),
                    pod: Some(pod_id.clone()),
                    stream,
                    kind: slipfork_world::OutputKind::Line,
                    line: line.to_owned(),
                })
            };
            match world.run_once_streaming(&pod, timeout_ms, &mut on_line) {
                Ok(code) => (
                    InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: Some(serde_json::json!({ "exit_code": code })),
                        error: None,
                    },
                    false,
                ),
                Err(fault) => {
                    eprintln!("slipfork-world-init: {fault}");
                    let code = if fault.is_timeout() {
                        "run-timeout"
                    } else {
                        "run-failed"
                    };
                    (failure_frame(id, code, &fault), false)
                }
            }
        }
        Request::Dial { id, .. } => (
            failure_frame(
                id,
                "dial-requires-socket",
                &Fault::new(
                    "dial must use the world control socket",
                    "send the dial request to `<pods>/control.sock`, not the supervisor stdin",
                ),
            ),
            false,
        ),
        Request::ConfigureS3 { id, config, .. } => match world.configure_s3(config) {
            Ok((listen, buckets)) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({
                        "listen": listen.to_string(),
                        "buckets": buckets,
                    })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "s3-configure-failed", &fault), false)
            }
        },
        Request::List { id, .. } => (
            InitFrame::Reply {
                v: PROTOCOL_VERSION,
                id,
                ok: Some(serde_json::json!({ "pods": world.list() })),
                error: None,
            },
            false,
        ),
        Request::Restart { id, service, .. } => match world.restart(service.as_deref()) {
            Ok(restarted) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "restarted": restarted })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "restart-failed", &fault), false)
            }
        },
        Request::Wait {
            id,
            pid,
            timeout_ms,
            ..
        } => match world.wait(pid, timeout_ms) {
            Ok(Some(code)) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "exit_code": code })),
                    error: None,
                },
                false,
            ),
            Ok(None) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "running": true })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "wait-failed", &fault), false)
            }
        },
        Request::Logs { id, .. } => (
            failure_frame(
                id,
                "logs-requires-socket",
                &Fault::new(
                    "logs must use the world control socket",
                    "send the logs request to `<pods>/control.sock`, not the supervisor stdin",
                ),
            ),
            false,
        ),
        Request::Exec { id, .. } => (
            failure_frame(
                id,
                "exec-requires-socket",
                &Fault::new(
                    "exec must use the world control socket",
                    "send the exec request to `<pods>/control.sock`, not the supervisor stdin",
                ),
            ),
            false,
        ),
        Request::ExecInput { id, .. } => (
            failure_frame(
                id,
                "exec-input-requires-socket",
                &Fault::new(
                    "exec input must use the world control socket",
                    "send input frames on the same socket as the exec request",
                ),
            ),
            false,
        ),
        Request::ExecPty { id, .. } => (
            failure_frame(
                id,
                "exec-pty-requires-socket",
                &Fault::new(
                    "a real PTY must use the world control socket",
                    "send the exec-pty request to `<pods>/control.sock`",
                ),
            ),
            false,
        ),
        Request::ExecResize { id, .. } => (
            failure_frame(
                id,
                "exec-resize-requires-socket",
                &Fault::new(
                    "PTY resize must use the world control socket",
                    "send resize frames on the same socket as the exec-pty request",
                ),
            ),
            false,
        ),
    };
    Ok((reply, shutdown))
}
