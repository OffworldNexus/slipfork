//! The daemon's `ingress.accept` handler: one accepted `:80` fd, routed and streamed.
//!
//! The stateless ingress forwards only the descriptor; every route decision is made here from the
//! live worlds' evaluated `Forkfile` tables, and each upstream connection is dialed through the
//! world's own `control.sock` (`Dial`) so traffic never has to be reachable from the host.

use std::collections::BTreeMap;
use std::io;
use std::net::TcpStream;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use slipfork_ingress::{IngressRegistry, serve_connection};

use super::Failure;

/// Serves the accepted connection against a snapshot of every world's route table.
pub(super) fn serve(
    registry: IngressRegistry,
    pods: BTreeMap<String, PathBuf>,
    fd: OwnedFd,
) -> Result<(), Failure> {
    let client = TcpStream::from(fd);
    serve_connection(client, &registry, |matched| {
        let pods_dir = pods.get(&matched.world).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("world `{}` is no longer running", matched.world),
            )
        })?;
        slipfork_world::control::dial_tcp(pods_dir, matched.port).map_err(io::Error::other)
    })
    .map_err(|error| Failure::new("ingress-failed", error.to_string()))
}
