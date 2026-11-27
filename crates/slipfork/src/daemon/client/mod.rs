//! The one thin client for `<runtime_dir>/daemon.sock`; it owns the daemon's lifetime.
//!
//! Every local verb connects here and nowhere else. When no daemon answers, the client spawns
//! `slipfork __daemon` detached and waits up to twenty seconds for the socket.

mod call;
mod error;
mod notice;
mod pty;
mod wire;

pub use error::DaemonError;
pub use notice::ConnectNotice;

use std::fs;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::identity::DaemonIdentity;
use super::socket_path;
use super::spawn::spawn_daemon;
use crate::world::HostEnv;

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
