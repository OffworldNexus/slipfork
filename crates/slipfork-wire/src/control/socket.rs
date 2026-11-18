//! The control socket's file name and path construction.
//!
//! The shared kernel pathname budget ([`slipfork_world::UNIX_SOCKET_PATH_MAX`]) lives in
//! `slipfork-world`, because `<state_dir>/http-ingress.sock` and `<runtime_dir>/daemon.sock`
//! share one `AF_UNIX` limit that also shapes a `WorldSpec`.

use std::path::{Path, PathBuf};

/// File name of the user control socket inside `slipforkd`'s explicit `--runtime-dir`.
pub const CONTROL_SOCKET_FILE: &str = "daemon.sock";

/// The control socket path for an explicitly supplied runtime directory.
pub fn control_socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(CONTROL_SOCKET_FILE)
}
