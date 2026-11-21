//! The one thin client for `<runtime_dir>/daemon.sock`; it owns the daemon's lifetime.
//!
//! Every local verb connects here and nowhere else. When no daemon answers, the client spawns
//! `slipfork __daemon` detached and waits up to three seconds for the socket.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

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
        self.next += 1;
        let id = format!("cli-{}", self.next);
        let bytes = wire::request(&id, method, params);
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| {
                DaemonError::new("daemon-unavailable", format!("send `{method}`: {error}"))
            })?;
        let mut reader = BufReader::new(&self.stream);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).map_err(|error| {
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
                continue;
            }
            if let Some(error) = response.error {
                return Err(DaemonError {
                    code: error.code,
                    message: error.message,
                });
            }
            return Ok(response.ok.unwrap_or(Value::Null));
        }
    }
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
