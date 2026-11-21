//! NDJSON control protocol between [`WorldSupervisor`] and `slipfork-world-init`.
//!
//! One newline-delimited JSON frame travels over the init's stdin/stdout. stdin is also the
//! `O_CLOEXEC` lifeline: when the supervisor closes it (or exits), PID 1 stops every pod and exits.
//! No pathname socket exists, so the 107-byte `sun_path` limit never applies.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::namespaces::NamespaceIdentity;
use crate::pod::PodSpec;

/// Frame version; both peers reject anything else.
pub const PROTOCOL_VERSION: u32 = 1;

/// A typed failure returned by the world init.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlError {
    /// Short machine-readable code (`launch-failed`, `invalid-pod`, …).
    pub code: String,
    /// One human line; never contains secret values.
    pub message: String,
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
}

impl Request {
    /// The request id echoed back by the init.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Launch { id, .. } | Self::StopAll { id, .. } => id,
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
        InitFrame::Ready { v, .. } | InitFrame::Reply { v, .. } | InitFrame::Stopped { v } => *v,
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
        Request::Launch { v, .. } | Request::StopAll { v, .. } => *v,
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
}
