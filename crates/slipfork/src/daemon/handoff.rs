//! `:80` handoff: forward accepted connections to `<runtime_dir>/daemon.sock`.
//!
//! Two entry modes share one direction. systemd starts the hidden mode once per accepted
//! connection (`Accept=yes`) with the socket on stdin/stdout; OpenRC starts it once with
//! `--listen <addr>` and the composition root binds that listener. Either way this process is
//! stateless: it forwards exactly one descriptor per connection, and the daemon's ingress module
//! still makes every routing decision.

use std::ffi::{OsStr, OsString};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use slipfork_ingress::IngressHandoff;

use super::socket_path;
use crate::world::HostEnv;

/// How long one handoff may block on the daemon socket before the listener moves on.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

/// The `sysctl` remedy when the kernel refuses a privileged port to this user.
const PORT_REMEDY: &str =
    "sudo slipfork daemon install --ingress  # or set net.ipv4.ip_unprivileged_port_start=0";

/// The hidden mode invocation shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// systemd handed this process one already-accepted connection on fd 0.
    Activated,
    /// Accept on this address for the lifetime of the process.
    Listen(SocketAddr),
}

impl Mode {
    /// Parses `[]` (socket activation) or exactly `--listen <addr>`.
    ///
    /// # Errors
    ///
    /// Returns the one-line usage message for an unknown flag or an unparseable address.
    pub fn parse(args: &[OsString]) -> Result<Self, String> {
        let mut iter = args.iter();
        let Some(first) = iter.next() else {
            return Ok(Self::Activated);
        };
        if first.as_os_str() != OsStr::new("--listen") {
            return Err(format!(
                "unknown argument `{}` for `slipfork __ingress`",
                first.to_string_lossy()
            ));
        }
        let raw = iter
            .next()
            .ok_or("`--listen` needs an address such as 127.0.0.1:80")?;
        if let Some(extra) = iter.next() {
            return Err(format!(
                "unexpected argument `{}` after `--listen`",
                extra.to_string_lossy()
            ));
        }
        let text = raw
            .to_str()
            .ok_or("the `--listen` address must be valid UTF-8")?;
        text.parse::<SocketAddr>()
            .map(Self::Listen)
            .map_err(|error| format!("cannot parse `{text}` as an IP socket address: {error}"))
    }
}

/// Runs the handoff with the listener the composition root already bound for `--listen` mode.
///
/// The `:80` ingress must run as the developer: a root-run handoff would connect to the root
/// user's `daemon.sock` and proxy nobody's worlds, so both modes fail closed instead.
pub fn run(host: &HostEnv, mode: Mode, listener: Option<TcpListener>) -> ExitCode {
    if host.uid == 0 {
        eprintln!("error: the :80 ingress must run as the developer, not root");
        eprintln!("next: sudo slipfork daemon install --ingress");
        return ExitCode::FAILURE;
    }
    match mode {
        Mode::Activated => match forward_activated(host) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(
                &format!("cannot hand the :80 connection to the daemon: {error}"),
                next_for(&error),
            ),
        },
        Mode::Listen(_) => {
            let Some(listener) = listener else {
                return report(
                    "the ingress has no bound listener",
                    "run `slipfork __ingress --listen 127.0.0.1:80`",
                );
            };
            match forward_listener(&listener, &socket_path(host)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => report(
                    &format!("cannot accept on the ingress listener: {error}"),
                    next_for(&error),
                ),
            }
        }
    }
}

fn report(message: &str, next: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("next: {next}");
    ExitCode::FAILURE
}

/// The remedy for an `EACCES` port bind/accept, or the generic daemon remedy otherwise.
fn next_for(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::PermissionDenied {
        PORT_REMEDY
    } else {
        "start the daemon with `slipfork up`, then reload the page"
    }
}

/// Forwards the single systemd-provided connection on fd 0.
fn forward_activated(host: &HostEnv) -> io::Result<()> {
    // SAFETY: systemd hands the accepted connection on fd 0 (`StandardInput=socket`); this
    // process owns it for its whole lifetime.
    let connection = unsafe { TcpStream::from_raw_fd(0) };
    hand_off(
        &connection,
        &socket_path(host),
        u64::from(std::process::id()),
    )
}

/// Accepts connections and hands each accepted descriptor to the daemon over `daemon.sock`.
///
/// Each connection gets a fresh `UnixStream` and the next sequence number, so the loop keeps no
/// per-connection state. A failed forward is reported and the listener keeps accepting: the
/// daemon may simply be restarting.
///
/// # Errors
///
/// Returns the fatal `accept(2)` failure that ends the loop.
fn forward_listener(listener: &TcpListener, daemon_socket: &Path) -> io::Result<()> {
    let mut sequence = 0_u64;
    for incoming in listener.incoming() {
        let connection = match incoming {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        sequence += 1;
        if let Err(error) = hand_off(&connection, daemon_socket, sequence) {
            eprintln!("error: cannot hand an ingress connection to the daemon: {error}");
            eprintln!("next: {}", next_for(&error));
        }
    }
    Ok(())
}

/// Sends one accepted descriptor to the daemon in a single `ingress.accept` frame.
fn hand_off(connection: &TcpStream, daemon_socket: &Path, sequence: u64) -> io::Result<()> {
    let daemon = UnixStream::connect(daemon_socket)?;
    daemon.set_write_timeout(Some(HANDOFF_TIMEOUT))?;
    IngressHandoff::new(sequence).send(&daemon, connection.as_fd())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn scratch(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-handoff-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create the handoff scratch directory on disk");
        root
    }

    fn host(uid: u32, runtime: &Path) -> HostEnv {
        HostEnv {
            exe: PathBuf::from("/nonexistent/slipfork"),
            path: Vec::new(),
            runtime_dir: runtime.to_path_buf(),
            state_dir: runtime.join("state"),
            store_root: runtime.join("store"),
            mounts: Vec::new(),
            arch: "x86_64",
            uid,
            gid: uid,
            resolv_conf: PathBuf::from("/etc/resolv.conf"),
            home: None,
            config_dir: runtime.join("config"),
            now_ms: 0,
            ingress_uid: uid,
            ingress_user: "developer".to_owned(),
            openrc: false,
            install_root: runtime.join("install"),
            hostname: "lab".to_owned(),
            display: false,
            ssh: false,
            opener: None,
        }
    }

    /// Accepts one daemon-socket connection within five seconds without blocking a test forever.
    fn accept_within(listener: &UnixListener) -> UnixStream {
        listener
            .set_nonblocking(true)
            .expect("make daemon.sock nonblocking");
        let deadline = Instant::now() + HANDOFF_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("restore blocking daemon.sock");
                    return stream;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the ingress never dialed daemon.sock"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("daemon.sock accept failed: {error}"),
            }
        }
    }

    #[test]
    fn listen_mode_forwards_each_connection_and_its_payload() {
        let runtime = scratch("forward");
        let daemon = UnixListener::bind(runtime.join("daemon.sock")).expect("bind daemon.sock");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let address = listener.local_addr().expect("read the loopback address");
        let host = host(1000, &runtime);
        std::thread::spawn(move || {
            let _ = run(&host, Mode::Listen(address), Some(listener));
        });
        for (sequence, payload) in [
            (
                1_u64,
                b"GET / HTTP/1.1\r\nHost: demo.localhost\r\n\r\n".as_slice(),
            ),
            (2_u64, b"PING".as_slice()),
        ] {
            let mut client = TcpStream::connect(address).expect("connect to the ingress");
            client.set_nodelay(true).expect("disable Nagle");
            client.write_all(payload).expect("write the request");
            client.flush().expect("flush the request");
            let daemon_side = accept_within(&daemon);
            daemon_side
                .set_read_timeout(Some(HANDOFF_TIMEOUT))
                .expect("bound the handoff read");
            let (handoff, mut forwarded) =
                IngressHandoff::accept(&daemon_side).expect("receive the forwarded descriptor");
            assert_eq!(handoff.sequence(), sequence);
            forwarded
                .set_read_timeout(Some(HANDOFF_TIMEOUT))
                .expect("bound the forwarded read");
            let mut received = vec![0_u8; payload.len()];
            forwarded
                .read_exact(&mut received)
                .expect("the forwarded descriptor carries the client bytes");
            assert_eq!(received, payload);
        }
        fs::remove_dir_all(&runtime).expect("remove the scratch directory");
    }

    #[test]
    fn root_cannot_run_the_80_handoff() {
        let runtime = scratch("root");
        let code = run(&host(0, &runtime), Mode::Activated, None);
        assert_eq!(code, ExitCode::FAILURE);
        fs::remove_dir_all(&runtime).expect("remove the scratch directory");
    }
}
