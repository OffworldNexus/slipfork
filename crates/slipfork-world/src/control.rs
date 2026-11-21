//! NDJSON control protocol between [`WorldSupervisor`] and `slipfork-world-init`.
//!
//! One newline-delimited JSON frame travels over the init's stdin/stdout. stdin is also the
//! `O_CLOEXEC` lifeline: when the supervisor closes it (or exits), PID 1 stops every pod and exits.
//! The same frames are also served over `<pods>/control.sock` (mode `0600`) so a separate
//! `slipfork down` / `slipfork restart` process can reach the world without the supervisor.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::namespaces::NamespaceIdentity;
use crate::pod::PodSpec;

/// Frame version; both peers reject anything else.
pub const PROTOCOL_VERSION: u32 = 1;

/// Socket name inside the world's tmpfs pod directory.
pub const CONTROL_SOCKET: &str = "control.sock";

/// `<pods>/control.sock`, refused before `bind(2)` when `sun_path` would overflow.
pub fn control_socket_path(pods_root: &std::path::Path) -> Result<std::path::PathBuf> {
    let path = pods_root.join(CONTROL_SOCKET);
    // `sockaddr_un.sun_path` is 108 bytes including the trailing NUL; keep a safety margin.
    if path.as_os_str().as_encoded_bytes().len() >= 100 {
        return Err(Error::Protocol {
            message: format!(
                "control socket `{}` exceeds the AF_UNIX path limit",
                path.display()
            ),
        });
    }
    Ok(path)
}

/// A typed failure returned by the world init.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlError {
    /// Short machine-readable code (`launch-failed`, `invalid-pod`, …).
    pub code: String,
    /// One human line; never contains secret values.
    pub message: String,
}

/// Which stream one [`InitFrame::Output`] line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Frames the world init writes to stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InitFrame {
    /// Handshake: namespaces are live and loopback is up.
    Ready {
        /// Protocol version.
        v: u32,
        /// PID as seen inside the child PID namespace (always `1`).
        init_pid: u32,
        /// Namespace inode identities for the supervisor to verify.
        namespaces: NamespaceIdentity,
    },
    /// Answer to one [`Request`].
    Reply {
        /// Protocol version.
        v: u32,
        /// The request id being answered.
        id: String,
        /// Result payload on success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<serde_json::Value>,
        /// Failure detail on error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ControlError>,
    },
    /// One live stdout/stderr line from a running [`Request::RunOnce`].
    Output {
        /// Protocol version.
        v: u32,
        /// The request id this line belongs to.
        id: String,
        /// Which stream produced the line.
        stream: Stream,
        /// The line, without its terminator.
        line: String,
    },
    /// Teardown is complete; the process exits `0` immediately after.
    Stopped {
        /// Protocol version.
        v: u32,
    },
}

/// Requests the supervisor writes to the init's stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum Request {
    /// Launch one OCI pod from an already-written tmpfs bundle.
    Launch {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Fully resolved pod value (config.json already written by the supervisor).
        pod: PodSpec,
    },
    /// Stop every pod, remove bundles, and reply before exiting on EOF.
    StopAll {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Stop every pod and exit; `slipfork down` sends this over the pathname socket.
    Shutdown {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Wait for one launched pod to exit and report its status code.
    Wait {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Host pid returned by `launch`.
        pid: i32,
        /// Upper bound in milliseconds.
        timeout_ms: u64,
    },
    /// Run one pod to completion (setup/hook step) while streaming its output.
    ///
    /// The init enforces `timeout_ms` itself: the single-threaded loop must never block forever
    /// on a silent step, because `StopAll`/`Shutdown` arrive on that same loop.
    RunOnce {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Fully resolved pod value (config.json already written by the supervisor).
        pod: PodSpec,
        /// Upper bound in milliseconds for the whole run, enforced inside the world.
        timeout_ms: u64,
    },
    /// Report every tracked pod and whether its process still exists.
    List {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Restart one pod from its existing tmpfs bundle, or every pod when `service` is `None`.
    Restart {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Pod id to restart; `None` restarts every pod.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
    },
}

impl Request {
    /// The request id echoed back by the init.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Launch { id, .. }
            | Self::StopAll { id, .. }
            | Self::Shutdown { id, .. }
            | Self::Restart { id, .. }
            | Self::List { id, .. }
            | Self::RunOnce { id, .. }
            | Self::Wait { id, .. } => id,
        }
    }
}

/// Encodes one frame plus its newline terminator.
pub fn encode(frame: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(frame).map_err(|source| Error::Protocol {
        message: format!("could not encode a control frame: {source}"),
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decodes one init frame, rejecting an unknown version.
pub fn decode_init_frame(line: &str) -> Result<InitFrame> {
    let frame: InitFrame = serde_json::from_str(line).map_err(|source| Error::Protocol {
        message: format!("bad init frame: {source}"),
    })?;
    let version = match &frame {
        InitFrame::Ready { v, .. }
        | InitFrame::Reply { v, .. }
        | InitFrame::Output { v, .. }
        | InitFrame::Stopped { v } => *v,
    };
    check_version(version)?;
    Ok(frame)
}

/// Decodes one supervisor request, rejecting an unknown version.
pub fn decode_request(line: &str) -> Result<Request> {
    let request: Request = serde_json::from_str(line).map_err(|source| Error::Protocol {
        message: format!("bad request frame: {source}"),
    })?;
    let version = match &request {
        Request::Launch { v, .. }
        | Request::StopAll { v, .. }
        | Request::Shutdown { v, .. }
        | Request::Restart { v, .. }
        | Request::List { v, .. }
        | Request::RunOnce { v, .. }
        | Request::Wait { v, .. } => *v,
    };
    check_version(version)?;
    Ok(request)
}

/// Fails closed on a protocol version the peer cannot speak.
pub fn check_version(version: u32) -> Result<()> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(Error::Protocol {
            message: format!("control protocol v{version} != v{PROTOCOL_VERSION}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pod::PodEnv;

    fn sample_pod() -> PodSpec {
        PodSpec {
            id: "api".to_owned(),
            rootfs: "/images/alpine".into(),
            workdir: "/project".to_owned(),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "true".to_owned()],
            inherited_env: Vec::new(),
            env: vec![PodEnv::new("PORT", "8000")],
            binds: Vec::new(),
            files: Vec::new(),
        }
    }

    #[test]
    fn launch_requests_round_trip_with_the_pod_value() {
        let request = Request::Launch {
            v: PROTOCOL_VERSION,
            id: "req-1".to_owned(),
            pod: sample_pod(),
        };
        let bytes = encode(&request).expect("encode");
        assert_eq!(bytes.last(), Some(&b'\n'));
        let decoded =
            decode_request(std::str::from_utf8(&bytes).expect("utf8").trim()).expect("decode");
        assert_eq!(decoded, request);
        assert_eq!(decoded.id(), "req-1");
    }

    #[test]
    fn ready_and_reply_frames_carry_the_kind_tag() {
        let identity = NamespaceIdentity::observe().expect("read /proc/self/ns");
        let ready = encode(&InitFrame::Ready {
            v: PROTOCOL_VERSION,
            init_pid: 1,
            namespaces: identity,
        })
        .expect("encode ready");
        let json = std::str::from_utf8(&ready).expect("utf8");
        assert!(json.contains("\"kind\":\"ready\""));
        let reply = InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: "req-1".to_owned(),
            ok: Some(serde_json::json!({"pid": 42})),
            error: None,
        };
        let bytes = encode(&reply).expect("encode reply");
        let decoded = decode_init_frame(std::str::from_utf8(&bytes).expect("utf8").trim())
            .expect("decode reply");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn list_reports_tracked_pods_running_or_exited() {
        let request = Request::List {
            v: PROTOCOL_VERSION,
            id: "req-7".to_owned(),
        };
        let bytes = encode(&request).expect("encode");
        let decoded =
            decode_request(std::str::from_utf8(&bytes).expect("utf8").trim()).expect("decode");
        assert_eq!(decoded, request);
        assert_eq!(decoded.id(), "req-7");
        let reply = InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: "req-7".to_owned(),
            ok: Some(serde_json::json!({"pods": [
                {"id": "pg", "pid": 42, "running": true},
                {"id": "svc", "pid": 7, "running": false}
            ]})),
            error: None,
        };
        let encoded = encode(&reply).expect("encode reply");
        let decoded = decode_init_frame(std::str::from_utf8(&encoded).expect("utf8").trim())
            .expect("decode reply");
        let InitFrame::Reply { ok: Some(ok), .. } = decoded else {
            panic!("expected a reply with an ok payload");
        };
        assert_eq!(ok["pods"][0]["running"], true);
        assert_eq!(ok["pods"][1]["running"], false);
    }

    #[test]
    fn rejects_unknown_versions_and_garbage_lines() {
        let wrong = Request::StopAll {
            v: PROTOCOL_VERSION + 1,
            id: "req-2".to_owned(),
        };
        let bytes = encode(&wrong).expect("encode");
        let line = std::str::from_utf8(&bytes).expect("utf8").trim();
        assert!(matches!(decode_request(line), Err(Error::Protocol { .. })));
        assert!(matches!(
            decode_init_frame("not json"),
            Err(Error::Protocol { .. })
        ));
    }

    #[test]
    fn shutdown_and_restart_frames_round_trip() {
        for request in [
            Request::Shutdown {
                v: PROTOCOL_VERSION,
                id: "req-9".to_owned(),
            },
            Request::Restart {
                v: PROTOCOL_VERSION,
                id: "req-10".to_owned(),
                service: Some("api".to_owned()),
            },
            Request::Restart {
                v: PROTOCOL_VERSION,
                id: "req-11".to_owned(),
                service: None,
            },
        ] {
            let bytes = encode(&request).expect("encode");
            let decoded =
                decode_request(std::str::from_utf8(&bytes).expect("utf8").trim()).expect("decode");
            assert_eq!(decoded, request);
            assert_eq!(decoded.id(), request.id());
        }
    }

    #[test]
    fn control_socket_paths_stay_inside_the_af_unix_limit() {
        let path = control_socket_path(std::path::Path::new("/run/user/1000/slipfork/pods/demo"))
            .expect("short tmpfs path");
        assert!(path.ends_with(CONTROL_SOCKET));
        let long = format!("/run/user/1000/{}", "a".repeat(120));
        let error = control_socket_path(std::path::Path::new(&long))
            .expect_err("overlong sun_path must be refused");
        assert!(matches!(error, Error::Protocol { .. }));
    }
}

#[cfg(test)]
mod socket_tests {
    use super::{
        CONTROL_SOCKET, PROTOCOL_VERSION, Request, control_socket_path, decode_request, encode,
    };
    use crate::error::Error;

    #[test]
    fn shutdown_and_restart_frames_round_trip() {
        for request in [
            Request::Shutdown {
                v: PROTOCOL_VERSION,
                id: "req-9".to_owned(),
            },
            Request::Restart {
                v: PROTOCOL_VERSION,
                id: "req-10".to_owned(),
                service: Some("api".to_owned()),
            },
            Request::Restart {
                v: PROTOCOL_VERSION,
                id: "req-11".to_owned(),
                service: None,
            },
        ] {
            let bytes = encode(&request).expect("encode");
            let decoded =
                decode_request(std::str::from_utf8(&bytes).expect("utf8").trim()).expect("decode");
            assert_eq!(decoded, request);
            assert_eq!(decoded.id(), request.id());
        }
    }

    #[test]
    fn control_socket_paths_stay_inside_the_af_unix_limit() {
        let path = control_socket_path(std::path::Path::new("/run/user/1000/slipfork/pods/demo"))
            .expect("short tmpfs path");
        assert!(path.ends_with(CONTROL_SOCKET));
        let long = format!("/run/user/1000/{}", "a".repeat(120));
        let error = control_socket_path(std::path::Path::new(&long))
            .expect_err("overlong sun_path must be refused");
        assert!(matches!(error, Error::Protocol { .. }));
    }
}
