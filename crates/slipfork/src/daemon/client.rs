//! The one thin client for `<runtime_dir>/daemon.sock`; it owns the daemon's lifetime.
//!
//! Every local verb connects here and nowhere else. When no daemon answers, the client spawns
//! `slipfork __daemon` detached and waits up to twenty seconds for the socket.

use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use base64::Engine as _;

use super::identity::DaemonIdentity;
use super::spawn::spawn_daemon;
use super::{socket_path, wire};
use crate::world::HostEnv;

/// One typed daemon failure; `message` may carry its own trailing `next:` remedy, which every
/// renderer prefers over its call site's generic fallback.
#[derive(Debug, Clone)]
pub struct DaemonError {
    pub code: String,
    pub message: String,
}

impl DaemonError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
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

/// The one notice the connect path shows when it has to start an ephemeral daemon.
///
/// It owns its own colour decision, so a boot renderer never re-renders the hint and the bytes
/// stay identical to the single line the CLI printed before this type existed.
#[derive(Debug, Clone, Copy)]
pub enum ConnectNotice {
    /// No packaged daemon answered, so this invocation starts a detached ephemeral one.
    Ephemeral {
        /// Whether ANSI styling is allowed for this invocation (`host.color`).
        color: bool,
    },
}

impl ConnectNotice {
    /// The exact bytes the ephemeral line has always printed.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Ephemeral { color } => {
                crate::ui::report::Palette::new(*color).meta(EPHEMERAL_HINT)
            }
        }
    }
}

impl fmt::Display for ConnectNotice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

/// The one ephemeral hint; the package enables the user service and the `:80` socket.
const EPHEMERAL_HINT: &str = "• daemon not installed; running ephemeral — the package enables it; on a tarball run `docs/install.sh`";

/// A connected daemon client with its own request sequence.
#[derive(Debug)]
pub struct DaemonClient {
    stream: UnixStream,
    socket: std::path::PathBuf,
    next: u64,
    interrupt: Option<&'static std::sync::atomic::AtomicBool>,
}

impl DaemonClient {
    /// Connects to the daemon, starting one detached when the socket is absent or stale.
    ///
    /// # Errors
    ///
    /// Fails after a twenty-second wait when a freshly spawned daemon never answers, reporting the
    /// startup log's own `error:`/`next:` pair when it wrote one.
    pub fn connect(
        host: &HostEnv,
        emit: &mut dyn FnMut(&ConnectNotice),
    ) -> Result<Self, DaemonError> {
        let socket = socket_path(host);
        match UnixStream::connect(&socket) {
            Ok(stream) => return Self::handshake(stream, socket, host),
            Err(error) if is_absent(&error) => {}
            Err(error) => {
                return Err(DaemonError::new(
                    "daemon-unavailable",
                    format!("connect `{}`: {error}", socket.display()),
                ));
            }
        }
        spawn_daemon(host)?;
        // One typed notice on the ephemeral autostart path; the caller prints its exact bytes.
        emit(&ConnectNotice::Ephemeral { color: host.color });
        let log = host.state_dir.join("daemon.log");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => return Self::handshake(stream, socket, host),
                Err(error) if Instant::now() < deadline && is_absent(&error) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    // A daemon that refused to start already wrote the exact remedy; surface it
                    // instead of a generic "did not answer".
                    if let Some(remedy) = startup_remedy(&log) {
                        return Err(DaemonError::new("daemon-startup", remedy));
                    }
                    return Err(DaemonError::new(
                        "daemon-unavailable",
                        format!(
                            "the slipfork daemon did not answer at `{}`: {error} (see `{}`)",
                            socket.display(),
                            log.display()
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
            Ok(stream) => Self::handshake(stream, socket, host).map(Some),
            Err(error) if is_absent(&error) => Ok(None),
            Err(error) => Err(DaemonError::new(
                "daemon-unavailable",
                format!("connect `{}`: {error}", socket.display()),
            )),
        }
    }

    /// Pings the daemon once before any verb runs, so a stale binary is refused with one remedy.
    ///
    /// The auto-spawn path passes: the daemon it just started is this very binary.
    fn handshake(stream: UnixStream, socket: PathBuf, host: &HostEnv) -> Result<Self, DaemonError> {
        let mut client = Self::new(stream, socket);
        client.verify_identity(host)?;
        Ok(client)
    }

    fn verify_identity(&mut self, host: &HostEnv) -> Result<(), DaemonError> {
        let identity = DaemonIdentity::new(&host.exe);
        let reply = match self.call_streaming("ping", json!({}), &mut |_| {}) {
            Ok(reply) => reply,
            // A daemon that cannot even answer `ping` predates this handshake.
            Err(error) if error.code == "unknown-method" => {
                return Err(identity.predates(None, &self.socket));
            }
            Err(error) => return Err(error),
        };
        identity.verify(&reply, &self.socket)
    }

    fn new(stream: UnixStream, socket: std::path::PathBuf) -> Self {
        Self {
            stream,
            socket,
            next: 0,
            interrupt: None,
        }
    }

    /// The live socket descriptor, so a relay can drop the connection when its reader vanishes.
    #[must_use]
    pub(crate) fn stream_fd(&self) -> RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.stream)
    }

    /// Aborts blocking polls as `"interrupted"` once `flag` is set by a `SIGINT` handler, while
    /// retrying benign `EINTR` wakeups such as `SIGWINCH`.
    #[must_use]
    pub fn with_interrupt(mut self, flag: &'static std::sync::atomic::AtomicBool) -> Self {
        self.interrupt = Some(flag);
        self
    }

    fn is_interrupted(&self) -> bool {
        self.interrupt
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
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
                    if let Some(value) = parse_response(text, &id, method, on_event)? {
                        return Ok(value);
                    }
                }
            }
        }
    }

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

/// Decodes a pre-descriptor daemon refusal frame into its exact typed error.
///
/// A PTY refusal (`shell-refused` for `s3`/unknown targets, `world-booting`, …) arrives as a plain
/// NDJSON `{"id":…,"error":{code,message}}` frame instead of the `pty` + `SCM_RIGHTS` payload.
fn refusal_frame(payload: &[u8]) -> Option<DaemonError> {
    let response: wire::Response = serde_json::from_slice(payload).ok()?;
    let failure = response.error?;
    Some(DaemonError::new(&failure.code, failure.message))
}

fn is_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// The daemon's own last `error:` line and the `next:` line that follows it, if it logged one.
fn startup_remedy(log: &Path) -> Option<String> {
    let text = fs::read_to_string(log).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let (index, error) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, line)| line.strip_prefix("error: ").map(|error| (index, error)))?;
    let next = lines.get(index + 1)?.strip_prefix("next: ")?;
    Some(format!("{error}\nnext: {next}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::startup_remedy;

    /// Real disk scratch space; `/tmp` is RAM-backed and never holds daemon state.
    fn log_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        PathBuf::from("/var/tmp").join(format!(
            "slipfork-daemon-log-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// A daemon that refused to start prints `error:` then `next:`; the last pair is the remedy.
    #[test]
    fn startup_remedy_returns_the_last_error_and_its_next_line() {
        let path = log_path("pair");
        fs::write(
            &path,
            "slipfork daemon: noise\n\
             error: open the world ledger: busy\nnext: check that the state directory is writable\n\
             error: state.db was written by a newer slipfork (migration m0009)\nnext: upgrade slipfork\n",
        )
        .expect("write the daemon log");
        assert_eq!(
            startup_remedy(&path).as_deref(),
            Some(
                "state.db was written by a newer slipfork (migration m0009)\nnext: upgrade slipfork"
            )
        );
        let _ = fs::remove_file(&path);
    }

    /// Without an `error:`/`next:` pair the caller keeps its generic message.
    #[test]
    fn startup_remedy_is_none_without_a_pair() {
        let path = log_path("bare");
        fs::write(
            &path,
            "slipfork daemon: another daemon already holds the runtime lock\n",
        )
        .expect("write the daemon log");
        assert_eq!(startup_remedy(&path), None);
        assert_eq!(
            startup_remedy(Path::new("/var/tmp/slipfork-daemon-log-missing")),
            None
        );
        let _ = fs::remove_file(&path);
    }
}
