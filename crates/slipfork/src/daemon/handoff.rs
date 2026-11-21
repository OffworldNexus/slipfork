//! Socket-activated `:80` handoff: forward the accepted connection to `daemon.sock`.
//!
//! systemd starts this hidden mode once per accepted connection (`Accept=yes`) with the socket on
//! stdin/stdout. The process is stateless: it forwards exactly one descriptor and exits, and the
//! daemon's ingress module still makes every routing decision.

use std::io;
use std::net::TcpStream;
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use slipfork_ingress::IngressHandoff;

use super::socket_path;
use crate::world::HostEnv;

/// Runs the per-connection handoff and exits.
pub fn run(host: &HostEnv) -> ExitCode {
    match forward(host) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot hand the :80 connection to the daemon: {error}");
            eprintln!("next: start the daemon with `slipfork up`, then reload the page");
            ExitCode::FAILURE
        }
    }
}

fn forward(host: &HostEnv) -> io::Result<()> {
    let socket = socket_path(host);
    // SAFETY: systemd hands the accepted connection on fd 0 (`StandardInput=socket`); this
    // process owns it for its whole lifetime.
    let connection = unsafe { TcpStream::from_raw_fd(0) };
    let daemon = UnixStream::connect(&socket)?;
    IngressHandoff::new(u64::from(std::process::id())).send(&daemon, connection.as_fd())
}
