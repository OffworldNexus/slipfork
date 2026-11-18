//! The single canonical binder for every pathname `AF_UNIX` socket `slipforkd` serves.
//!
//! Both `<state_dir>/http-ingress.sock` and `<runtime_dir>/daemon.sock` are user-private
//! (`0600`) listener sockets with identical lifecycle rules: a stale socket file left by a dead
//! daemon is replaced, a socket a live daemon still answers on is refused (never stolen), and a
//! path beyond the kernel's `sun_path` budget fails closed before anything is bound.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use slipfork_wire::control::{UNIX_SOCKET_PATH_MAX, unix_socket_path_fits};

use crate::EdgeError;

/// Which daemon-owned socket is being bound (used verbatim in diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketRole {
    /// `<state_dir>/http-ingress.sock` (`H-011` #4 Variant a).
    HttpIngress,
    /// `<runtime_dir>/daemon.sock` (`ADR-0008` §4).
    DaemonControl,
}

impl SocketRole {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::HttpIngress => "http ingress",
            Self::DaemonControl => "daemon control",
        }
    }
}

/// Create the socket's parent directory, replace a stale socket file, bind it, and set `0600`.
///
/// A pathname longer than [`UNIX_SOCKET_PATH_MAX`] is rejected up front, because the kernel would
/// otherwise fail with an unrelated `ENAMETOOLONG` after a partially prepared directory.
pub(crate) fn bind_private(path: &Path, role: SocketRole) -> Result<UnixListener, EdgeError> {
    if !unix_socket_path_fits(path) {
        return Err(EdgeError::SocketPath {
            role: role.label(),
            path: path.display().to_string(),
            limit: UNIX_SOCKET_PATH_MAX,
        });
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|err| EdgeError::SocketDir {
            role: role.label(),
            path: dir.display().to_string(),
            message: err.to_string(),
        })?;
    }
    replace_stale_socket(path, role)?;
    let listener = UnixListener::bind(path).map_err(|err| EdgeError::SocketBind {
        role: role.label(),
        path: path.display().to_string(),
        message: err.to_string(),
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|err| {
        EdgeError::SocketBind {
            role: role.label(),
            path: path.display().to_string(),
            message: format!("cannot restrict the socket to 0600: {err}"),
        }
    })?;
    Ok(listener)
}

/// Replace a stale socket file, but never steal one a live daemon still answers on.
fn replace_stale_socket(path: &Path, role: SocketRole) -> Result<(), EdgeError> {
    if !path.exists() {
        return Ok(());
    }
    if UnixStream::connect(path).is_ok() {
        return Err(EdgeError::SocketInUse {
            role: role.label(),
            path: path.display().to_string(),
        });
    }
    fs::remove_file(path).map_err(|err| socket_remove_error(path, role, err))
}

fn socket_remove_error(path: &Path, role: SocketRole, err: io::Error) -> EdgeError {
    EdgeError::SocketBind {
        role: role.label(),
        path: path.display().to_string(),
        message: format!("cannot remove the stale socket: {err}"),
    }
}
