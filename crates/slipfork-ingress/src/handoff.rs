//! The `ingress.accept` control-frame codec and typed handoff.
//!
//! The ingress sender and the daemon decoder share this one codec: the frame is one
//! NEWLINE-delimited JSON object (`{"v":1,"id":"<seq>","method":"ingress.accept","params":{}}`) and
//! the accepted connection descriptor travels alongside it in the same `sendmsg`.
//!
//! The handoff is one direction only: one frame carrying exactly one descriptor.

use std::io;
use std::net::TcpStream;
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;

use serde_json::{Value, json};

use crate::fdpass::{recv_fd, send_fd};

/// Method name of the control frame that carries the accepted connection.
pub const ACCEPT_METHOD: &str = "ingress.accept";

/// The typed `ingress.accept` frame that carries one accepted `:80` connection.
///
/// The `id` is a daemon-issued sequence number rendered as a decimal string, so the shared codec
/// never needs to coerce or escape caller data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressHandoff {
    sequence: u64,
}

impl IngressHandoff {
    /// Builds the handoff for one daemon-issued sequence number.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self { sequence }
    }

    /// The daemon-issued sequence number carried by this frame.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Encode the exact NEWLINE-delimited control frame.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut frame = json!({
            "v": 1,
            "id": self.sequence.to_string(),
            "method": ACCEPT_METHOD,
            "params": {},
        })
        .to_string()
        .into_bytes();
        frame.push(b'\n');
        frame
    }

    /// Decode one control frame, rejecting unknown versions, methods, and ids.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidData`] for malformed JSON or an unexpected frame shape.
    pub fn decode(frame: &[u8]) -> io::Result<Self> {
        let value: Value = serde_json::from_slice(frame).map_err(|_| invalid())?;
        if value.get("v").and_then(Value::as_u64) != Some(1)
            || value.get("method").and_then(Value::as_str) != Some(ACCEPT_METHOD)
        {
            return Err(invalid());
        }
        let sequence = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .parse::<u64>()
            .map_err(|_| invalid())?;
        Ok(Self { sequence })
    }

    /// Sends the frame and the accepted connection descriptor in one `sendmsg`.
    ///
    /// # Errors
    ///
    /// Propagates [`send_fd`] failures.
    pub fn send(&self, socket: &UnixStream, connection: BorrowedFd<'_>) -> io::Result<()> {
        send_fd(socket, &self.encode(), Some(connection))
    }

    /// Receives one handoff and turns the descriptor back into the accepted connection.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidData`] when the frame is malformed or carries no descriptor.
    pub fn accept(socket: &UnixStream) -> io::Result<(Self, TcpStream)> {
        let (frame, descriptor) = recv_fd(socket)?;
        let handoff = Self::decode(&frame)?;
        let descriptor = descriptor.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ingress.accept frame carried no descriptor",
            )
        })?;
        Ok((handoff, TcpStream::from(descriptor)))
    }
}

/// One uniform malformed-frame error.
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "ingress.accept frame is malformed",
    )
}
