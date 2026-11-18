//! Wire configuration shared by the world engines and their per-world PID 1
//! (`src/bin/slipfork-world-init.rs`, `H-011`).
//!
//! `clone_world` writes one `0600` JSON document into the world's control directory and spawns the
//! helper with that path as its single argument. The document carries **no** secrets: only the
//! world's bind mounts, the `netns.sock` path, and the optional in-netns `$BASE_URL` bridge.
//!
//! The guest-path constants and the pure boot helpers below are the single owner of the world's
//! guest layout: every engine maps host state onto the same `/.slipfork/*` paths and the same
//! synthesized `/etc/{passwd,group}`.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{EngineError, MountMode, WorldMount};

/// Guest mount of the world's CoW `state_dir` (`<subvol>/state`).
pub(crate) const WORLD_STATE_GUEST: &str = "/.slipfork/state";
/// Guest mount of the world's `oci` bundle directory (control directory, never the subvolume).
pub(crate) const OCI_BUNDLES_GUEST: &str = "/.slipfork/oci";
/// Guest mount of the world's `crun` state root (control directory, never the subvolume).
pub(crate) const OCI_RUN_GUEST: &str = "/.slipfork/run";
/// Guest mount prefix of extracted backing-service OCI rootfs trees.
pub(crate) const OCI_IMAGES_GUEST: &str = "/.slipfork/images";
/// Guest path of the synthesized `/etc/passwd`.
pub(crate) const PASSWD_GUEST: &str = "/.slipfork/etc/passwd";
/// Guest path of the synthesized `/etc/group`.
pub(crate) const GROUP_GUEST: &str = "/.slipfork/etc/group";
/// Guest path of `slipforkd`'s ingress socket after `pivot_root`.
pub(crate) const INGRESS_GUEST: &str = "/.slipfork/http-ingress.sock";

/// File name of the world init configuration inside the world directory.
pub const WORLD_INIT_CONFIG_FILE: &str = "world-init.json";

/// File name of the readiness marker written once every listener is bound.
pub const WORLD_INIT_READY_FILE: &str = "init.ready";

/// File name of the world's `AF_UNIX` dial socket.
pub const WORLD_INIT_SOCKET_FILE: &str = "netns.sock";

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
    /// Guest path the host ingress socket is bound at after `pivot_root`.
    pub guest_ingress_sock: PathBuf,
}

/// Read-only `L0` rootfs the world PID 1 merges and pivots into (`ADR-0009` §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInitRootfs {
    /// Extracted `slipfork-world` stage, shared read-only by every world.
    pub app_rootfs: PathBuf,
    /// Empty directory tree that contributes mount points to the overlay.
    pub mountpoints: PathBuf,
    /// Directory the two-lowerdir overlay is mounted at.
    pub merged: PathBuf,
}

/// Complete boot configuration for one persistent world init process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInitConfig {
    /// Pathname `AF_UNIX` socket answering `dial_world_tcp` requests (mode `0600`).
    pub netns_sock: PathBuf,
    /// Read-only overlay + `pivot_root` inputs (`ADR-0009` §3.1).
    pub rootfs: WorldInitRootfs,
    /// Bind mounts to establish inside the merged root before `pivot_root`.
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

/// Translate one [`WorldMount`] into the wire form the world PID 1 establishes.
pub(crate) fn world_init_mount(mount: &WorldMount) -> WorldInitMount {
    WorldInitMount {
        host_path: mount.host_path.clone(),
        guest_path: mount.guest_path.clone(),
        read_only: mount.mode == MountMode::ReadOnly,
    }
}

/// Guest mount of one extracted OCI rootfs, keyed by its immutable image digest.
pub(crate) fn image_guest_rootfs(digest: &str) -> Result<PathBuf, EngineError> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| EngineError::InvalidWorldSpec {
            reason: format!(
                "backing-service image digest '{digest}' must be `sha256:<64 lowercase hex>`"
            ),
        })?;
    Ok(PathBuf::from(OCI_IMAGES_GUEST).join(hex).join("rootfs"))
}

/// Synthesize read-only `/etc/passwd` + `/etc/group` carrying the invoking `$UID:$GID`
/// (`ADR-0009` §3.1 rule 2): `initdb` fails with `could not look up effective user ID` without it.
///
/// The files live in the world's control directory, never in the world subvolume, so a seal never
/// captures host identity data.
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
