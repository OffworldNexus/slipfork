//! Request framing and the two poll loops: NDJSON streaming, and duplex stdin pumping.

use std::io::{Read, Write};
use std::os::fd::RawFd;

use base64::Engine as _;
use serde_json::Value;

use super::super::wire;
use super::wire::parse_response;
use super::{DaemonClient, DaemonError};

impl DaemonClient {
    /// Sends one request, forwards every `event` frame, and returns the final `ok` payload.
    ///
    /// # Errors
    ///
    /// Propagates transport failures and the daemon's typed error.
    pub fn call_streaming(
        &mut self,
        method: &str,
        params: Value,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, DaemonError> {
        let id = self.begin(method, params)?;
        self.read_replies(&id, method, on_event)
    }

    /// Sends one request and pumps the injected stdin as base64 `input` frames.
    ///
    /// `input_fd` is polled alongside the daemon socket, so no reader thread and no ambient
    /// `std::io::stdin()` live in this client. Used by `shell`, `exec`, `db`, and `redis`.
    pub fn call_duplex(
        &mut self,
        method: &str,
        params: Value,
        input: &mut dyn Read,
        input_fd: RawFd,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, DaemonError> {
        let id = self.begin(method, params)?;
        let mut lines = slipfork_world::control::FrameLines::new();
        let mut input_done = false;
        loop {
            if self.is_interrupted() {
                return Err(DaemonError::new(
                    "interrupted",
                    format!("`{method}` interrupted"),
                ));
            }
            let mut fds = [
                libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(&self.stream),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: input_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid `pollfd`s live across the call; 100ms bounds the wait.
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
            if ret < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(DaemonError::new(
                    "daemon-unavailable",
                    format!("poll `{method}`: {error}"),
                ));
            }
            let readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
            if !input_done && fds[1].revents & readable != 0 {
                let mut buffer = [0_u8; 8192];
                match input.read(&mut buffer) {
                    Ok(0) => {
                        self.send_input(&id, "")?;
                        input_done = true;
                    }
                    Ok(read) => {
                        let data =
                            base64::engine::general_purpose::STANDARD.encode(&buffer[..read]);
                        self.send_input(&id, &data)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        self.send_input(&id, "")?;
                        input_done = true;
                    }
                }
            }
            if fds[0].revents & readable != 0 {
                let mut buffer = [0_u8; 8192];
                let read = match self.stream.read(&mut buffer) {
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(DaemonError::new(
                            "daemon-unavailable",
                            format!("read `{method}` reply: {error}"),
                        ));
                    }
                };
                if read == 0 {
                    return Err(DaemonError::new(
                        "daemon-unavailable",
                        format!(
                            "the daemon closed the connection during `{method}` (see `{}`)",
                            self.socket.display()
                        ),
                    ));
                }
                lines.extend(&buffer[..read]);
                while let Some(raw) = lines.next_line() {
                    let text = std::str::from_utf8(&raw).map_err(|error| {
                        DaemonError::new("protocol", format!("non-UTF-8 daemon frame: {error}"))
                    })?;
                    if let Some(value) = parse_response(text, &id, on_event)? {
                        return Ok(value);
                    }
                }
            }
        }
    }

    /// Forwards this process's stdin to the daemon as base64 `input` frames; EOF sends an empty one.
    pub(super) fn send_input(&mut self, id: &str, data: &str) -> Result<(), DaemonError> {
        let bytes = wire::input(id, data);
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| DaemonError::new("daemon-unavailable", format!("send stdin: {error}")))
    }

    pub(super) fn begin(&mut self, method: &str, params: Value) -> Result<String, DaemonError> {
        self.next += 1;
        let id = format!("cli-{}", self.next);
        let bytes = wire::request(&id, method, params);
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| {
                DaemonError::new("daemon-unavailable", format!("send `{method}`: {error}"))
            })?;
        Ok(id)
    }

    pub(super) fn read_replies(
        &mut self,
        id: &str,
        method: &str,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, DaemonError> {
        let mut lines = slipfork_world::control::FrameLines::new();
        loop {
            if self.is_interrupted() {
                return Err(DaemonError::new(
                    "interrupted",
                    format!("`{method}` interrupted"),
                ));
            }
            let mut pollfd = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&self.stream),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call; 100ms keeps Ctrl-C responsive.
            let ret = unsafe { libc::poll(&mut pollfd, 1, 100) };
            if ret < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(DaemonError::new(
                    "daemon-unavailable",
                    format!("poll `{method}`: {error}"),
                ));
            }
            let readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
            if pollfd.revents & readable == 0 {
                continue;
            }
            let mut buffer = [0_u8; 8192];
            let read = match self.stream.read(&mut buffer) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(DaemonError::new(
                        "daemon-unavailable",
                        format!("read `{method}` reply: {error}"),
                    ));
                }
            };
            if read == 0 {
                return Err(DaemonError::new(
                    "daemon-unavailable",
                    format!(
                        "the daemon closed the connection during `{method}` (see `{}`)",
                        self.socket.display()
                    ),
                ));
            }
            lines.extend(&buffer[..read]);
            while let Some(raw) = lines.next_line() {
                let text = std::str::from_utf8(&raw).map_err(|error| {
                    DaemonError::new("protocol", format!("non-UTF-8 daemon frame: {error}"))
                })?;
                if let Some(value) = parse_response(text, id, on_event)? {
                    return Ok(value);
                }
            }
        }
    }
}
