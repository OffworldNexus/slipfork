//! The PTY verbs: descriptor receipt, resize forwarding, and the final exit reply.

use std::io::Write;
use std::os::fd::OwnedFd;

use serde_json::Value;

use super::super::wire;
use super::wire::refusal_frame;
use super::{DaemonClient, DaemonError};

impl DaemonClient {
    /// Sends one `shell` request and receives the world's PTY master descriptor.
    ///
    /// The daemon sends exactly one `SCM_RIGHTS` payload (`pty` plus the `/dev/pts/N` master)
    /// before any NDJSON reply; the returned id keys every later [`Self::send_resize`] and the
    /// final [`Self::finish_pty`].
    ///
    /// # Errors
    ///
    /// Propagates transport failures and the daemon's typed shell refusal.
    pub fn call_pty(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(String, OwnedFd), DaemonError> {
        let id = self.begin(method, params)?;
        let (payload, descriptor) =
            slipfork_world::fdpass::recv_fd(&self.stream).map_err(|error| {
                DaemonError::new(
                    "daemon-unavailable",
                    format!("receive the PTY descriptor for `{method}`: {error}"),
                )
            })?;
        if payload != b"pty" {
            return Err(refusal_frame(&payload).unwrap_or_else(|| {
                DaemonError::new(
                    "protocol",
                    format!(
                        "expected the PTY descriptor payload, got `{}`",
                        String::from_utf8_lossy(&payload)
                    ),
                )
            }));
        }
        let Some(descriptor) = descriptor else {
            return Err(DaemonError::new(
                "protocol",
                "the daemon sent a PTY payload without a descriptor",
            ));
        };
        Ok((id, descriptor))
    }

    /// Forwards one terminal resize to the daemon, which relays it to the world PTY.
    ///
    /// # Errors
    ///
    /// Propagates transport failures.
    pub fn send_resize(&mut self, id: &str, cols: u16, rows: u16) -> Result<(), DaemonError> {
        let bytes = wire::resize(id, cols, rows);
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| {
                DaemonError::new("daemon-unavailable", format!("send resize: {error}"))
            })
    }

    /// Reads the final `ok {"exit":n}` after a PTY session ended.
    ///
    /// # Errors
    ///
    /// Propagates transport failures and the daemon's typed error.
    pub fn finish_pty(&mut self, id: &str, method: &str) -> Result<Value, DaemonError> {
        self.read_replies(id, method, &mut |_| {})
    }
}
