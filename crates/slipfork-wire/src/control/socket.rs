//! The one `0600` pathname-socket budget every daemon-owned `AF_UNIX` socket shares.
//!
//! `<state_dir>/http-ingress.sock` and `<runtime_dir>/daemon.sock` are user-private listener
//! sockets with the same kernel budget: a path longer than [`UNIX_SOCKET_PATH_MAX`] is rejected
//! before anything is bound, because the kernel would otherwise fail with an unrelated
//! `ENAMETOOLONG` after a partially prepared directory.

use std::path::{Path, PathBuf};

/// File name of the user control socket inside `slipforkd`'s explicit `--runtime-dir`.
pub const CONTROL_SOCKET_FILE: &str = "daemon.sock";

/// Longest pathname an `AF_UNIX` socket can bind: `sun_path` holds 108 bytes including the NUL.
pub const UNIX_SOCKET_PATH_MAX: usize = 107;

/// `true` when `path` fits the kernel's `sun_path` buffer.
///
/// A pathname `AF_UNIX` socket is the only transport that crosses `CLONE_NEWNET` without a relay
/// (`H-011` #3), but the kernel rejects longer paths with `ENAMETOOLONG`/`EINVAL`.
pub fn unix_socket_path_fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() <= UNIX_SOCKET_PATH_MAX
}

/// The control socket path for an explicitly supplied runtime directory.
pub fn control_socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(CONTROL_SOCKET_FILE)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn pathname_budget_is_exact() {
        let under = format!("/{}", "a".repeat(UNIX_SOCKET_PATH_MAX - 1));
        let over = format!("/{}", "a".repeat(UNIX_SOCKET_PATH_MAX));
        assert_eq!(under.len(), UNIX_SOCKET_PATH_MAX);
        assert!(unix_socket_path_fits(Path::new(&under)));
        assert!(!unix_socket_path_fits(Path::new(&over)));
    }
}
