//! Wire configuration shared by the world engines and their per-world PID 1
//! (`src/bin/slipfork-world-init.rs`, `H-011`).
//!
//! `clone_world` writes one `0600` JSON document into the world's control directory and spawns the
//! helper with that path as its single argument. The document carries **no** secrets: only the
//! world's `netns.sock` path, the namespace publication file, and the optional in-netns
//! `$BASE_URL` bridge.
//!
//! There is no world root filesystem: the world is one shared `user`/`mount`/`net`/`ipc`/`pid`
//! namespace set (the init is PID 1 and owns `/proc`), and every service — app component or
//! backing service — is its own `crun` container with its own rootfs. All host paths are visible
//! 1:1 in that mount namespace, so bundles, rootfses, volume subvolumes, and identity files are
//! addressed by their host paths with no `/.slipfork/*` remap.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use slipfork_world::EngineError;
use slipfork_world::rootfs::WorldRootfs;

/// File name of the world init configuration inside the world control directory.
pub const WORLD_INIT_CONFIG_FILE: &str = "world-init.json";

/// File name of the readiness marker written once every listener is bound.
pub const WORLD_INIT_READY_FILE: &str = "init.ready";

/// File name of the world's `AF_UNIX` dial socket.
pub const WORLD_INIT_SOCKET_FILE: &str = "netns.sock";

/// File name of the namespace publication the engine reads for pod bundles.
pub const WORLD_INIT_NAMESPACES_FILE: &str = "namespaces.json";

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
    /// File the init writes its `user`/`net`/`ipc` namespace paths into.
    pub namespaces_file: PathBuf,
    /// Directories the init covers with a private (`0700`, `16m`) tmpfs before `init.ready`.
    ///
    /// The engine writes every OCI bundle and `crun` state root beneath these, so resolved secrets
    /// live only in world-private memory the kernel reclaims on PID 1 exit.
    #[serde(default)]
    pub tmpfs_dirs: Vec<PathBuf>,
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

/// Synthesize read-only `/etc/passwd` + `/etc/group` carrying the invoking `$UID:$GID`
/// (`ADR-0009` §3.1 rule 2): `initdb` fails with `could not look up effective user ID` without it.
///
/// The files live in the world's control directory, never in the world subvolume, so a seal never
/// captures host identity data, and containers bind them by host path.
pub(crate) fn synthesize_identity_files(
    dir: &Path,
    rootfs: &WorldRootfs,
    home: &Path,
    uid: u32,
    gid: u32,
) -> Result<(PathBuf, PathBuf), EngineError> {
    let etc = dir.join("etc");
    fs::create_dir_all(&etc).map_err(|err| EngineError::Io {
        path: etc.display().to_string(),
        message: err.to_string(),
    })?;
    let user = host_identity_name("/etc/passwd", uid, 2).unwrap_or_else(|| "slipfork".to_owned());
    let group = host_identity_name("/etc/group", gid, 2).unwrap_or_else(|| "slipfork".to_owned());

    let passwd = with_identity_line(
        crate::rootfs::read_guest_file(&rootfs.app_rootfs, "/etc/passwd"),
        uid,
        &format!(
            "{user}:x:{uid}:{gid}:slipfork world user:{}:/bin/sh",
            home.display()
        ),
    );
    let group_file = with_identity_line(
        crate::rootfs::read_guest_file(&rootfs.app_rootfs, "/etc/group"),
        gid,
        &format!("{group}:x:{gid}:"),
    );
    let passwd_path = etc.join("passwd");
    let group_path = etc.join("group");
    fs::write(&passwd_path, passwd).map_err(|err| EngineError::Io {
        path: passwd_path.display().to_string(),
        message: err.to_string(),
    })?;
    fs::write(&group_path, group_file).map_err(|err| EngineError::Io {
        path: group_path.display().to_string(),
        message: err.to_string(),
    })?;
    Ok((passwd_path, group_path))
}

fn host_identity_name(path: &str, id: u32, field: usize) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    raw.lines().find_map(|line| {
        let parts: Vec<&str> = line.split(':').collect();
        (parts.get(field)?.parse::<u32>().ok()? == id).then(|| parts[0].to_owned())
    })
}

fn with_identity_line(source: Option<Vec<u8>>, id: u32, line: &str) -> Vec<u8> {
    let mut bytes = source.unwrap_or_default();
    let present = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
        text.lines().any(|existing| {
            existing
                .split(':')
                .nth(2)
                .is_some_and(|field| field.parse::<u32>() == Ok(id))
        })
    });
    if !present {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}
