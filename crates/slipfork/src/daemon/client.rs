//! The one thin client for `<runtime_dir>/daemon.sock`; it owns the daemon's lifetime.
//!
//! Every local verb connects here and nowhere else. When no daemon answers, the client spawns
//! `slipfork __daemon` detached and waits up to three seconds for the socket.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use base64::Engine as _;

use super::{socket_path, wire};
use crate::world::HostEnv;

/// One typed daemon failure; `message` never embeds a `next:` remedy.
#[derive(Debug, Clone)]
pub struct DaemonError {
    pub code: String,
    pub message: String,
}

impl DaemonError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DaemonError {}

/// A connected daemon client with its own request sequence.
#[derive(Debug)]
pub struct DaemonClient {
    stream: UnixStream,
    socket: std::path::PathBuf,
    next: u64,
}

impl DaemonClient {
    /// Connects to the daemon, starting one detached when the socket is absent or stale.
    ///
    /// # Errors
    ///
    /// Fails after a three-second wait when a freshly spawned daemon never answers.
    pub fn connect(host: &HostEnv) -> Result<Self, DaemonError> {
        let socket = socket_path(host);
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(Self::new(stream, socket)),
            Err(error) if is_absent(&error) => {}
            Err(error) => {
                return Err(DaemonError::new(
                    "daemon-unavailable",
                    format!("connect `{}`: {error}", socket.display()),
                ));
            }
        }
        spawn_daemon(host)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => return Ok(Self::new(stream, socket)),
                Err(error) if Instant::now() < deadline && is_absent(&error) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(DaemonError::new(
                        "daemon-unavailable",
                        format!(
                            "the slipfork daemon did not answer at `{}`: {error} (see `{}`)",
                            socket.display(),
                            host.state_dir.join("daemon.log").display()
                        ),
                    ));
                }
            }
        }
    }

    /// Connects only when a daemon already answers; read-only verbs never spawn one.
    ///
    /// # Errors
    ///
    /// Propagates transport failures other than an absent or refused socket.
    pub fn connect_existing(host: &HostEnv) -> Result<Option<Self>, DaemonError> {
        let socket = socket_path(host);
        match UnixStream::connect(&socket) {
            Ok(stream) => Ok(Some(Self::new(stream, socket))),
            Err(error) if is_absent(&error) => Ok(None),
            Err(error) => Err(DaemonError::new(
                "daemon-unavailable",
                format!("connect `{}`: {error}", socket.display()),
            )),
        }
    }

    fn new(stream: UnixStream, socket: std::path::PathBuf) -> Self {
        Self {
            stream,
            socket,
            next: 0,
        }
    }

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
                let read = self.stream.read(&mut buffer).map_err(|error| {
                    DaemonError::new(
                        "daemon-unavailable",
                        format!("read `{method}` reply: {error}"),
                    )
                })?;
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
                    if let Some(value) = parse_response(text, &id, method, on_event)? {
                        return Ok(value);
                    }
                }
            }
        }
    }

    fn send_input(&mut self, id: &str, data: &str) -> Result<(), DaemonError> {
        let bytes = wire::input(id, data);
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| DaemonError::new("daemon-unavailable", format!("send stdin: {error}")))
    }

    fn begin(&mut self, method: &str, params: Value) -> Result<String, DaemonError> {
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

    fn read_replies(
        &mut self,
        id: &str,
        method: &str,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, DaemonError> {
        let mut lines = slipfork_world::control::FrameLines::new();
        loop {
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
                    return Err(DaemonError::new(
                        "interrupted",
                        format!("`{method}` interrupted"),
                    ));
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
            let read = self.stream.read(&mut buffer).map_err(|error| {
                DaemonError::new(
                    "daemon-unavailable",
                    format!("read `{method}` reply: {error}"),
                )
            })?;
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
                if let Some(value) = parse_response(text, id, method, on_event)? {
                    return Ok(value);
                }
            }
        }
    }
}

/// Forwards this process's stdin to the daemon as base64 `input` frames; EOF sends an empty one.
/// Parses one daemon frame; `Ok(Some(value))` is the final reply, `Ok(None)` an event.
fn parse_response(
    line: &str,
    id: &str,
    method: &str,
    on_event: &mut dyn FnMut(&Value),
) -> Result<Option<Value>, DaemonError> {
    let response: wire::Response = serde_json::from_str(line.trim()).map_err(|error| {
        DaemonError::new("protocol", format!("malformed daemon frame: {error}"))
    })?;
    if response.v != wire::VERSION {
        return Err(DaemonError::new(
            "protocol",
            format!(
                "the daemon speaks protocol v{}; this client speaks v{}",
                response.v,
                wire::VERSION
            ),
        ));
    }
    if response.id != id {
        return Err(DaemonError::new(
            "protocol",
            format!("unexpected daemon reply id `{}` for `{id}`", response.id),
        ));
    }
    if let Some(event) = response.event {
        on_event(&event);
        return Ok(None);
    }
    if let Some(error) = response.error {
        return Err(DaemonError {
            code: error.code,
            message: error.message,
        });
    }
    let _ = method;
    Ok(Some(response.ok.unwrap_or(Value::Null)))
}

fn is_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn spawn_daemon(host: &HostEnv) -> Result<(), DaemonError> {
    fs::create_dir_all(&host.runtime_dir).map_err(|error| {
        DaemonError::new(
            "daemon-unavailable",
            format!("create `{}`: {error}", host.runtime_dir.display()),
        )
    })?;
    fs::create_dir_all(&host.state_dir).map_err(|error| {
        DaemonError::new(
            "daemon-unavailable",
            format!("create `{}`: {error}", host.state_dir.display()),
        )
    })?;
    let log_path = host.state_dir.join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| {
            DaemonError::new(
                "daemon-unavailable",
                format!("open `{}`: {error}", log_path.display()),
            )
        })?;
    Command::new(&host.exe)
        .arg("__daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| {
            DaemonError::new(
                "daemon-unavailable",
                format!("start `slipfork __daemon`: {error}"),
            )
        })?;
    Ok(())
}
