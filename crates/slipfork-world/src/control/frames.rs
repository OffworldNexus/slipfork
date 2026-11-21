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
    /// The merged `<pods>/logs/<id>.log` capture (replayed by `logs`).
    Combined,
}

impl Stream {
    /// The wire name shared by the daemon and the CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Combined => "combined",
        }
    }
}

/// What one output line represents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    /// A normal stdout/stderr line.
    #[default]
    Line,
    /// A `── <id> started|restarted ──` divider.
    Divider,
    /// No more lines for this pod in the current replay.
    Eof,
}

impl OutputKind {
    /// The wire name shared by the daemon and the CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Divider => "divider",
            Self::Eof => "eof",
        }
    }
}

/// Frames the world init writes to its control channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
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
    /// One live output line from a run-once, `logs`, or `exec` request.
    Output {
        /// Protocol version.
        v: u32,
        /// The request id this line belongs to.
        id: String,
        /// Pod that produced the line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pod: Option<String>,
        /// Which stream produced the line.
        stream: Stream,
        /// Whether this is pod output, a divider, or the end of a pod's replay.
        #[serde(default)]
        kind: OutputKind,
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

/// One newline scanner for every NDJSON frame and captured log.
///
/// The pending buffer is the only place a partial line lives, so callers never re-scan
/// delimiters or hold a second copy. `feed` bounds the buffer at `cap` bytes by emitting
/// oversized pieces as their own line.
#[derive(Debug, Default)]
pub struct FrameLines {
    pending: Vec<u8>,
}

impl FrameLines {
    /// An empty scanner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends raw bytes.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// Bytes held for an incomplete trailing line.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Pops one complete line without its `\n`.
    pub fn next_line(&mut self) -> Option<Vec<u8>> {
        let position = self.pending.iter().position(|byte| *byte == b'\n')?;
        let rest = self.pending.split_off(position + 1);
        let mut line = std::mem::replace(&mut self.pending, rest);
        line.truncate(position);
        Some(line)
    }

    /// Appends bytes and returns every completed line, cutting oversized lines at `cap`.
    pub fn feed(&mut self, bytes: &[u8], cap: usize) -> Vec<Vec<u8>> {
        self.extend(bytes);
        let mut lines = Vec::new();
        loop {
            if let Some(line) = self.next_line() {
                lines.push(line);
                continue;
            }
            if self.pending.len() > cap {
                lines.push(self.pending.drain(..cap).collect());
                continue;
            }
            break;
        }
        lines
    }

    /// Takes a trailing partial line (used when a producer stops without a newline).
    pub fn take_partial(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }
}
