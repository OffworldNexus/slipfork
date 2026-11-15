//! Wire configuration shared by [`crate::ReflinkNetnsEngine`] and its per-world PID 1
//! (`src/bin/slipfork-world-init.rs`, `H-011`).
//!
//! `clone_world` writes one `0600` JSON document into the world directory and spawns the helper
//! with that path as its single argument. The document carries **no** secrets: only the world's
//! bind mounts, the `netns.sock` path, and the optional in-netns `$BASE_URL` bridge.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the world init configuration inside the world directory.
pub const WORLD_INIT_CONFIG_FILE: &str = "world-init.json";

/// File name of the readiness marker written once every listener is bound.
pub const WORLD_INIT_READY_FILE: &str = "init.ready";

/// File name of the world's `AF_UNIX` dial socket.
pub const WORLD_INIT_SOCKET_FILE: &str = "netns.sock";

/// Longest pathname an `AF_UNIX` socket can bind: `sun_path` holds 108 bytes including the NUL.
pub const UNIX_SOCKET_PATH_MAX: usize = 107;

/// `true` when `path` fits the kernel's `sun_path` buffer.
///
/// A pathname `AF_UNIX` socket is the only transport that crosses `CLONE_NEWNET` without a relay
/// (`H-011` #3), but the kernel rejects longer paths with `ENAMETOOLONG`/`EINVAL`. Both the world
/// directory and `slipforkd`'s `http-ingress.sock` derive from `<state_root>`, so an overlong state
/// root fails closed during validation instead of producing a half-usable world.
pub fn unix_socket_path_fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() <= UNIX_SOCKET_PATH_MAX
}

/// One bind mount the world init establishes inside the world mount namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInitMount {
    /// Directory on the host.
    pub host_path: PathBuf,
    /// Path the directory is mounted at inside the world.
    pub guest_path: PathBuf,
    /// Mount read-only (`mount -o remount,bind,ro`) instead of read-write.
    pub read_only: bool,
}

/// In-netns loopback listener spliced to `slipforkd`'s `AF_UNIX` ingress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInitIngress {
    /// Port bound on `127.0.0.1` inside the world network namespace.
    pub port: u16,
    /// Pathname `AF_UNIX` socket served by `slipforkd` (`<state_root>/http-ingress.sock`).
    pub host_ingress_sock: PathBuf,
}

/// Complete boot configuration for one persistent world init process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInitConfig {
    /// Pathname `AF_UNIX` socket answering `dial_world_tcp` requests (mode `0600`).
    pub netns_sock: PathBuf,
    /// Bind mounts to establish before any listener is bound.
    pub mounts: Vec<WorldInitMount>,
    /// Optional in-netns `$BASE_URL` bridge.
    pub local_ingress: Option<WorldInitIngress>,
}

impl WorldInitConfig {
    /// Readiness marker path: the world init writes it once every listener is bound.
    pub fn ready_path(&self) -> PathBuf {
        self.netns_sock
            .parent()
            .map(|dir| dir.join(WORLD_INIT_READY_FILE))
            .unwrap_or_else(|| PathBuf::from(WORLD_INIT_READY_FILE))
    }
}
