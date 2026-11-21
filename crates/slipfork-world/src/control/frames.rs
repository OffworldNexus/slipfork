//! The typed NDJSON frames shared by the supervisor, the init, and separate clients.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::namespaces::NamespaceIdentity;

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
    /// Answer to one request.
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
    /// One live stdout/stderr line from a running run-once request.
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
