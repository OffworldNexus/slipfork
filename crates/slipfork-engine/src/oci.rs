//! OCI runtime bundles for daemonless `crun` backing services **and** shared-namespace service
//! pods (`ADR-0009` §2–§3, `ARCHITECTURE.md` §3.3).
//!
//! This module is the **only** place a container `config.json` is synthesized. It is pure
//! `#![forbid(unsafe_code)]` data: the caller supplies already-resolved guest paths (the image
//! rootfs, the CoW state subdirectories, verified read-only dumps) and the invoking `$UID:$GID`.
//! The `crun` probe in `docs/design/world-base-image.md` §7 fixes the shape:
//!
//! - a **backing service** declares identity `uidMappings`/`gidMappings` (`$UID -> $UID`) and
//!   explicit `user`, `pid`, `mount`, `ipc`, and `uts` namespaces — **never** a `network`
//!   namespace, because every backing container shares the world's `127.0.0.1`;
//! - a **service pod** inherits the world init's `ipc` namespace through the engine's `nsenter`
//!   prefix (an OCI `path` join cannot be opened from inside the world) and creates an
//!   identity-mapped `user` namespace plus fresh `pid`, `mount`, and `uts`, so it listens on the
//!   world's `127.0.0.1` and never declares a `network` namespace;
//! - read-only image rootfs (`root.readonly = true`);
//! - read-only `/etc/passwd` + `/etc/group` overlays when the container needs host identity
//!   (`initdb` resolves `getpwuid($UID)`);
//! - `--cgroup-manager=disabled` belongs to the `crun` invocation, not this document.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;
use slipfork_world::EngineError;
use slipfork_world::rootfs::{BackingServiceKey, OciRootfs};

/// Directory name of the synthesized OCI bundle inside the world's OCI state directory.
pub const OCI_CONFIG_FILE: &str = "config.json";

/// One bind mount inside the container rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerMount {
    /// Absolute path in the world mount namespace that `crun` binds from.
    pub source: PathBuf,
    /// Absolute destination inside the container rootfs.
    pub destination: PathBuf,
    /// Mount read-only instead of read-write.
    pub read_only: bool,
}

/// Everything needed to synthesize one OCI bundle.
#[derive(Debug, Clone)]
pub struct ContainerBundle<'a> {
    /// Directory the bundle is written to (host path inside the world's OCI state dir).
    pub bundle_dir: &'a Path,
    /// Path the runtime resolves as the read-only container `/` (host path, visible 1:1 in the
    /// world mount namespace).
    pub image_rootfs: &'a Path,
    /// Container command and arguments.
    pub argv: &'a [String],
    /// Effective container environment.
    pub env: &'a BTreeMap<String, String>,
    /// Invoking developer uid.
    pub uid: u32,
    /// Invoking developer gid.
    pub gid: u32,
    /// State and read-only bind mounts.
    pub mounts: &'a [ContainerMount],
    /// Namespaces joined **by path** from the world's shared init instead of created fresh
    /// (`("user" | "network" | "ipc", path)`); joined kinds are never emitted twice.
    pub namespace_joins: &'a [(&'a str, &'a Path)],
    /// `process.cwd` inside the container (absolute, and bound in by `mounts` when host-owned).
    pub cwd: &'a Path,
    /// Guest path of the synthesized `/etc/passwd`, when the container needs host identity.
    pub passwd_guest: Option<&'a Path>,
    /// Guest path of the synthesized `/etc/group`, when the container needs host identity.
    pub group_guest: Option<&'a Path>,
}

/// Write the OCI runtime bundle for one `crun` container.
pub fn write_bundle(bundle: &ContainerBundle<'_>) -> io::Result<()> {
    fs::create_dir_all(bundle.bundle_dir)?;
    let joins_userns = bundle
        .namespace_joins
        .iter()
        .any(|(kind, _path)| *kind == "user");
    // Identity mappings apply only when the container *creates* its user namespace. A container
    // joining the world init's `user` namespace inherits that map and must not declare its own.
    let mut linux = json!({ "namespaces": namespaces(bundle) });
    if !joins_userns {
        linux["uidMappings"] =
            json!([{ "containerID": bundle.uid, "hostID": bundle.uid, "size": 1 }]);
        linux["gidMappings"] =
            json!([{ "containerID": bundle.gid, "hostID": bundle.gid, "size": 1 }]);
    }
    let config = json!({
        "ociVersion": "1.0.2",
        "hostname": format!("slipfork-{}", bundle.uid),
        "root": { "path": bundle.image_rootfs, "readonly": true },
        "process": {
            "terminal": false,
            "user": { "uid": bundle.uid, "gid": bundle.gid },
            "args": bundle.argv,
            "env": bundle
                .env
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>(),
            "cwd": bundle.cwd,
            "noNewPrivileges": true,
        },
        "mounts": container_mounts(bundle),
        "linux": linux,
    });
    let bytes = serde_json::to_vec_pretty(&config).map_err(io::Error::other)?;
    fs::write(bundle.bundle_dir.join(OCI_CONFIG_FILE), bytes)
}

/// The container's `linux.namespaces`: every joined namespace first (with its injected `path`),
/// then the freshly created kinds the container still owns.
///
/// A backing service declares no joins, so it creates `user`/`pid`/`mount`/`ipc`/`uts` and — by
/// design — **no** `network` namespace: it shares the world's `127.0.0.1` (`ADR-0009` §2), so
/// `postgres:18` and `redis:8` talk to an `alpine:3.21` app. A service pod joins `user`, `network`,
/// and `ipc` by path and creates the rest.
fn namespaces(bundle: &ContainerBundle<'_>) -> Vec<serde_json::Value> {
    // A namespace named in `namespace_joins` is **inherited** from the engine's `nsenter` prefix
    // and therefore omitted here: the OCI `path` form cannot be opened from inside the world
    // (`/proc/1/ns/*` is owned by the init's capabilities), while the runtime itself already runs
    // inside those namespaces. Every other kind is created fresh.
    ["user", "pid", "mount", "ipc", "uts"]
        .into_iter()
        .filter(|kind| {
            !bundle
                .namespace_joins
                .iter()
                .any(|(inherited, _path)| inherited == kind)
        })
        .map(|kind| json!({ "type": kind }))
        .collect()
}

fn container_mounts(bundle: &ContainerBundle<'_>) -> Vec<serde_json::Value> {
    let mut mounts = vec![
        json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/dev/pts",
            "type": "devpts",
            "source": "devpts",
            // A gid outside the identity map is unmapped and makes `mount(2)` fail with `EINVAL`.
            "options": [
                "nosuid",
                "noexec",
                "newinstance",
                "ptmxmode=0666",
                "mode=0620",
                format!("gid={}", bundle.gid)
            ]
        }),
        json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
        }),
        json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777"]
        }),
        json!({
            "destination": "/run",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=755"]
        }),
        json!({
            "destination": "/sys",
            "type": "bind",
            "source": "/sys",
            "options": ["ro", "rbind"]
        }),
    ];
    // Identity files are only bound when the container needs host `$UID:$GID` resolution: a
    // backing service joins no namespaces and synthesizes its own `/etc/passwd`, while a shared-ns
    // service pod inherits the world init's identity and carries no synthesized file.
    if let Some(passwd_guest) = bundle.passwd_guest {
        mounts.push(bind_mount(passwd_guest, Path::new("/etc/passwd"), true));
    }
    if let Some(group_guest) = bundle.group_guest {
        mounts.push(bind_mount(group_guest, Path::new("/etc/group"), true));
    }
    for mount in bundle.mounts {
        mounts.push(bind_mount(
            &mount.source,
            &mount.destination,
            mount.read_only,
        ));
    }
    mounts
}

fn bind_mount(source: &Path, destination: &Path, read_only: bool) -> serde_json::Value {
    let options = if read_only {
        vec!["ro", "rbind"]
    } else {
        vec!["rw", "rbind"]
    };
    json!({
        "destination": destination,
        "type": "bind",
        "source": source,
        "options": options,
    })
}

/// `true` when two container paths are equal or one contains the other.
///
/// Backing-service state must never be shadowed by a read-only client mount (or vice versa), so
/// [`slipfork_world::EngineError::InvalidWorldSpec`] is raised before any container exists.
pub fn paths_overlap(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Validate one container destination path (`/data`, `/var/lib/postgresql/data`).
pub fn validate_container_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "container path '{}' must be absolute",
            path.display()
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "container path '{}' must not contain '..'",
            path.display()
        ));
    }
    if path == Path::new("/") {
        return Err("container path '/' would shadow the image rootfs".to_owned());
    }
    Ok(())
}

/// Validate one state subdirectory (`pgdata/main`) before it is joined under the world state dir.
pub fn validate_state_subdir(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        return Err(format!(
            "state subdirectory '{}' must be relative to the world state dir",
            path.display()
        ));
    }
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "state subdirectory '{}' must be a plain relative path (no '..')",
            path.display()
        ));
    }
    Ok(())
}

/// Stable `crun` container id derived from the world sequence and service id.
pub(crate) fn container_id(world_seq: u64, service_id: &str) -> String {
    let slug: String = service_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "service" } else { slug };
    format!("sf{world_seq}-{}", &slug[..slug.len().min(48)])
}

/// Refuse a container mount destination the backing-service image does not carry.
pub(crate) fn ensure_container_destination(
    image: &OciRootfs,
    image_key: &BackingServiceKey,
    container_path: &Path,
) -> Result<(), EngineError> {
    let relative = container_path
        .strip_prefix("/")
        .map_err(|_| EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' must be absolute",
                container_path.display()
            ),
        })?;
    if !image.rootfs.join(relative).exists() {
        return Err(EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' does not exist in backing-service image {image_key}; \
                 the image rootfs must carry the mount destination",
                container_path.display()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_declares_identity_mappings_and_no_network_namespace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]);
        let argv = vec!["postgres".to_owned()];
        write_bundle(&ContainerBundle {
            bundle_dir: dir.path(),
            image_rootfs: Path::new("/var/tmp/slipfork/cache/oci/abc/rootfs"),
            argv: &argv,
            env: &env,
            uid: 1000,
            gid: 1000,
            mounts: &[],
            namespace_joins: &[],
            cwd: Path::new("/"),
            passwd_guest: Some(Path::new("/.slipfork/etc/passwd")),
            group_guest: Some(Path::new("/.slipfork/etc/group")),
        })
        .expect("write bundle");

        let raw = fs::read_to_string(dir.path().join(OCI_CONFIG_FILE)).expect("config");
        let config: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let namespaces = config["linux"]["namespaces"]
            .as_array()
            .expect("namespaces")
            .iter()
            .map(|namespace| namespace["type"].as_str().expect("type"))
            .collect::<Vec<_>>();
        assert_eq!(namespaces, ["user", "pid", "mount", "ipc", "uts"]);
        assert_eq!(config["linux"]["uidMappings"][0]["containerID"], 1000);
        assert_eq!(config["linux"]["uidMappings"][0]["hostID"], 1000);
        assert_eq!(config["root"]["readonly"], true);
        assert_eq!(config["process"]["user"]["uid"], 1000);
    }

    #[test]
    fn overlaps_and_escaping_container_paths_are_rejected() {
        assert!(paths_overlap(Path::new("/data"), Path::new("/data/db")));
        assert!(!paths_overlap(Path::new("/data"), Path::new("/dumps")));
        assert!(validate_container_path(Path::new("/data")).is_ok());
        assert!(validate_container_path(Path::new("data")).is_err());
        assert!(validate_container_path(Path::new("/data/../etc")).is_err());
        assert!(validate_container_path(Path::new("/")).is_err());
        assert!(validate_state_subdir(Path::new("pgdata/main")).is_ok());
        assert!(validate_state_subdir(Path::new("/pgdata")).is_err());
        assert!(validate_state_subdir(Path::new("../escape")).is_err());
    }
}
