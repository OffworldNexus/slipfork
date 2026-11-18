//! The 6-rule World Image Contract (`ADR-0009` §4.2) with the exact §5 user-facing diagnostics.
//!
//! Rules 1, 3, 4, and 6 are static: they are re-checked on every materialization (including cache
//! hits) so a `snow.yml` port edit can never bypass a cached rootfs. Rules 2 and 5 inspect the
//! extracted tree, which is immutable for a given digest.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::rootfs::abi::{ElfAbi, ElfProbe, host_cache_abi_mismatch, suggested_base};
use crate::rootfs::dockerfile::{Stage, argv, key_value_pairs};
use crate::rootfs::pin::{CrunPin, crun_pin};
use crate::rootfs::{
    BaseImageError, CRUN_GUEST_PATH, HostCacheRequest, OsRelease, RuntimeAssets,
    WORLD_INIT_GUEST_PATH, WorldRootfsRequest, read_guest_file, resolve_guest_path, sha256_hex,
};

/// Root-only privilege-drop wrappers forbidden by World Image Contract #3.
const PRIVILEGE_WRAPPERS: &[&str] = &["gosu", "su-exec", "sudo", "chpst"];

/// `ENV` keys whose baked value violates single-origin injection (`ADR-0005`).
const BAKED_ORIGIN_KEYS: &[&str] = &[
    "BASE_URL",
    "DATABASE_URL",
    "REDIS_URL",
    "AWS_S3_ENDPOINT_URL",
];

/// Rules 3, 4, and 6 over the parsed target stage. Returns the declared `EXPOSE` ports.
pub(crate) fn validate_static(
    request: &WorldRootfsRequest,
    stage: &Stage,
) -> Result<BTreeSet<u16>, BaseImageError> {
    let component = if request.ports.component.is_empty() {
        request.image_id.clone()
    } else {
        request.ports.component.clone()
    };

    // Rule 3 — never run a root-only privilege-drop entrypoint as $UID:$GID over a read-only /.
    for keyword in ["ENTRYPOINT", "CMD"] {
        let Some(args) = stage.instruction(keyword) else {
            continue;
        };
        let tokens = argv(args);
        if tokens
            .iter()
            .any(|token| PRIVILEGE_WRAPPERS.contains(&token.as_str()))
        {
            return Err(BaseImageError::RootAtRuntimeEntrypoint {
                stage: stage_name(stage),
                entrypoint: tokens,
                uid: request.uid,
                gid: request.gid,
                component,
            });
        }
    }

    // Rule 4 — no baked origin/service URLs or host user paths.
    for instruction in stage.instructions_for("ENV") {
        for (key, value) in key_value_pairs(&instruction.args) {
            if is_baked_origin(&key, &value) {
                return Err(BaseImageError::BakedOriginOrHostPath {
                    stage: stage_name(stage),
                    what: format!("ENV `{key}={value}`"),
                    component,
                });
            }
        }
    }
    for instruction in stage.instructions_for("WORKDIR") {
        let dir = instruction.args.trim().trim_matches('"');
        if contains_host_path(dir) {
            return Err(BaseImageError::BakedOriginOrHostPath {
                stage: stage_name(stage),
                what: format!("host path `{dir}`"),
                component,
            });
        }
    }

    // Rule 6 — every declared port must be served by a snow.yml component/ingress.
    let declared = declared_ports(stage)?;
    let allowed = request.ports.allowed_ports();
    for (port, instruction) in &declared {
        if allowed.contains(port) {
            continue;
        }
        return Err(BaseImageError::ExposeIngressMismatch {
            image: request.image_id.clone().into(),
            instruction,
            port: *port,
            component: component.into(),
            internal: request
                .ports
                .internal_ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
            ingress: if request.ports.ingress_ports.is_empty() {
                "none".into()
            } else {
                request
                    .ports
                    .ingress_ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
                    .into()
            },
            suggested: allowed.iter().next().copied().unwrap_or(*port),
        });
    }

    Ok(declared.into_iter().map(|(port, _)| port).collect())
}

/// Rules 2, 5, and `/.slipfork/bin` integrity over the extracted app stage.
pub(crate) fn validate_extracted(
    request: &WorldRootfsRequest,
    stage: &Stage,
    rootfs: &Path,
    elf_probe: &ElfProbe,
) -> Result<(OsRelease, ElfAbi, BTreeSet<u16>), BaseImageError> {
    let os_release = detect_distro(stage, rootfs)?;

    if let Some((path, fix)) = first_backing_service(rootfs) {
        return Err(BaseImageError::EmbeddedBackingService {
            stage: stage_name(stage),
            path,
            fix,
        });
    }

    verify_runtime_binary(rootfs, CRUN_GUEST_PATH)?;
    verify_runtime_binary(rootfs, WORLD_INIT_GUEST_PATH)?;
    let pin = crun_pin(&request.target_arch)?;
    verify_crun_digest(rootfs, pin)?;
    verify_world_init_digest(rootfs, &request.runtime)?;

    let shell = resolve_guest_path(rootfs, "/bin/sh").ok_or_else(|| missing_shell(stage))?;
    let elf_abi = elf_probe.probe(rootfs, &shell)?;
    let exposed_ports = declared_ports(stage)?
        .into_iter()
        .map(|(port, _)| port)
        .collect();
    Ok((os_release, elf_abi, exposed_ports))
}

/// Resolve the `--host-caches` mounts after a fail-closed ELF/libc ABI probe (`ADR-0009` §2.3).
pub(crate) fn validate_host_caches(
    request: &WorldRootfsRequest,
    stage: &Stage,
    world_abi: &ElfAbi,
    distro: &str,
    elf_probe: &ElfProbe,
) -> Result<Vec<crate::WorldMount>, BaseImageError> {
    let HostCacheRequest { host_shell, mounts } = &request.host_caches;
    if mounts.is_empty() {
        return Ok(Vec::new());
    }
    if host_shell.as_os_str().is_empty() {
        return Err(BaseImageError::InvalidRequest {
            reason: "`--host-caches` needs an injected host ABI witness (host_shell)".to_owned(),
        });
    }
    if mounts
        .iter()
        .any(|mount| mount.mode != crate::MountMode::ReadOnly)
    {
        return Err(BaseImageError::InvalidRequest {
            reason: "`--host-caches` mounts must be read-only; never expose a writable host cache"
                .to_owned(),
        });
    }
    let host_abi = elf_probe.probe(Path::new("/"), host_shell)?;
    if host_cache_abi_mismatch(&host_abi, world_abi) {
        return Err(BaseImageError::HostCacheAbiMismatch {
            arch: host_abi.arch.clone().into(),
            libc: host_abi.libc,
            version: host_abi.libc_version.clone().into(),
            suggested: suggested_base(&host_abi),
            host: Box::new(host_abi),
            world: Box::new(world_abi.clone()),
            stage: stage_name(stage).into(),
            distro: distro.into(),
        });
    }
    Ok(mounts.clone())
}

fn missing_shell(stage: &Stage) -> BaseImageError {
    BaseImageError::UnsupportedDistro {
        stage: stage_name(stage),
        line: stage.line,
        detail: "missing /etc/os-release + /bin/sh".to_owned(),
    }
}

fn stage_name(stage: &Stage) -> String {
    stage
        .name
        .clone()
        .unwrap_or_else(|| format!("Dockerfile:{}", stage.line))
}

fn is_baked_origin(key: &str, value: &str) -> bool {
    BAKED_ORIGIN_KEYS.contains(&key)
        || value.contains(".localhost")
        || value.contains("://localhost")
        || contains_host_path(value)
}

fn contains_host_path(value: &str) -> bool {
    value
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ';' | ':' | '='))
        .any(|token| {
            token.starts_with("/home/")
                || token.starts_with("/Users/")
                || token == "/home"
                || token == "/Users"
        })
}

/// `EXPOSE` + `HEALTHCHECK` ports, each tagged with the instruction that declared it.
fn declared_ports(stage: &Stage) -> Result<Vec<(u16, &'static str)>, BaseImageError> {
    let mut ports: Vec<(u16, &'static str)> = Vec::new();
    for instruction in stage.instructions_for("EXPOSE") {
        for token in instruction.args.split_whitespace() {
            let port = token.split('/').next().unwrap_or_default();
            if let Ok(port) = port.parse::<u16>() {
                ports.push((port, "EXPOSE"));
            }
        }
    }
    for instruction in stage.instructions_for("HEALTHCHECK") {
        for port in healthcheck_ports(&instruction.args) {
            ports.push((port, "HEALTHCHECK"));
        }
    }
    Ok(ports)
}

/// TCP ports referenced by `scheme://host:port` or `localhost:<port>` in a healthcheck command.
fn healthcheck_ports(args: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    let mut rest = args;
    while let Some(index) = rest.find("://") {
        let after = &rest[index + 3..];
        let authority = after
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '/' | '?'))
            .next()
            .unwrap_or_default();
        if let Some((_, port)) = authority.rsplit_once(':')
            && let Ok(port) = port.parse::<u16>()
        {
            ports.push(port);
        }
        rest = after;
    }
    for needle in ["localhost:", "127.0.0.1:"] {
        let mut rest = args;
        while let Some(index) = rest.find(needle) {
            let digits: String = rest[index + needle.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(port) = digits.parse::<u16>() {
                ports.push(port);
            }
            rest = &rest[index + needle.len()..];
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Parse `<rootfs>/etc/os-release` and enforce the supported table.
fn detect_distro(stage: &Stage, rootfs: &Path) -> Result<OsRelease, BaseImageError> {
    let bytes = read_guest_file(rootfs, "/etc/os-release").ok_or_else(|| missing_shell(stage))?;
    let Some(os_release) = parse_os_release(&bytes) else {
        return Err(BaseImageError::UnsupportedDistro {
            stage: stage_name(stage),
            line: stage.line,
            detail: "missing /etc/os-release + /bin/sh".to_owned(),
        });
    };
    if !is_supported(&os_release) {
        return Err(BaseImageError::UnsupportedDistro {
            stage: stage_name(stage),
            line: stage.line,
            detail: format!("detected distro '{}'", os_release.distro_label()),
        });
    }
    if resolve_guest_path(rootfs, "/bin/sh").is_none() || !is_executable_elf(rootfs) {
        return Err(missing_shell(stage));
    }
    Ok(os_release)
}

fn is_executable_elf(rootfs: &Path) -> bool {
    let Some(path) = resolve_guest_path(rootfs, "/bin/sh") else {
        return false;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return false;
    };
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return false;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return false;
    };
    bytes.starts_with(b"\x7fELF")
}

fn parse_os_release(bytes: &[u8]) -> Option<OsRelease> {
    let text = String::from_utf8_lossy(bytes);
    let mut id = None;
    let mut version_id = String::new();
    let mut version_codename = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "ID" => id = Some(value.to_ascii_lowercase()),
            "VERSION_ID" => version_id = value.to_owned(),
            "VERSION_CODENAME" => version_codename = value.to_owned(),
            _ => {}
        }
    }
    Some(OsRelease {
        id: id?,
        version_id,
        version_codename,
    })
}

fn is_supported(os_release: &OsRelease) -> bool {
    match os_release.id.as_str() {
        "debian" => {
            matches!(os_release.version_codename.as_str(), "bookworm" | "trixie")
                || matches!(os_release.version_id.as_str(), "12" | "13")
        }
        "ubuntu" => {
            matches!(os_release.version_codename.as_str(), "jammy" | "noble")
                || matches!(os_release.version_id.as_str(), "22.04" | "24.04")
        }
        "alpine" => ["3.20", "3.21"].iter().any(|supported| {
            os_release.version_id == *supported
                || os_release
                    .version_id
                    .strip_prefix(supported)
                    .is_some_and(|rest| rest.starts_with('.'))
        }),
        _ => false,
    }
}

/// First backing-service daemon installed in the app stage (rule 5).
fn first_backing_service(rootfs: &Path) -> Option<(String, String)> {
    const FIXED: &[(&str, &str)] = &[
        (
            "/usr/bin/postgres",
            "remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in snow.yml",
        ),
        (
            "/usr/local/bin/postgres",
            "remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in snow.yml",
        ),
        (
            "/usr/bin/redis-server",
            "remove `redis` server installation from Dockerfile and declare `resources.redis_clusters` in snow.yml",
        ),
        (
            "/usr/local/bin/redis-server",
            "remove `redis` server installation from Dockerfile and declare `resources.redis_clusters` in snow.yml",
        ),
        (
            "/usr/bin/weed",
            "remove `seaweedfs` server installation from Dockerfile and declare `resources.s3_buckets` in snow.yml",
        ),
        (
            "/usr/local/bin/weed",
            "remove `seaweedfs` server installation from Dockerfile and declare `resources.s3_buckets` in snow.yml",
        ),
        (
            "/usr/bin/dockerd",
            "remove the `dockerd` installation from Dockerfile; Slipfork never runs a Docker daemon inside a world",
        ),
        (
            "/usr/local/bin/dockerd",
            "remove the `dockerd` installation from Dockerfile; Slipfork never runs a Docker daemon inside a world",
        ),
    ];
    for (guest, fix) in FIXED {
        if rootfs.join(guest.trim_start_matches('/')).is_file() {
            return Some(((*guest).to_owned(), (*fix).to_owned()));
        }
    }
    let postgres_dir = rootfs.join("usr/lib/postgresql");
    if let Ok(entries) = std::fs::read_dir(&postgres_dir) {
        let mut versions: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        versions.sort();
        for version in versions {
            if version.join("bin/postgres").is_file() {
                let guest = format!(
                    "/usr/lib/postgresql/{}/bin/postgres",
                    version.file_name().unwrap_or_default().to_string_lossy()
                );
                return Some((
                    guest,
                    "remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in snow.yml"
                        .to_owned(),
                ));
            }
        }
    }
    None
}

fn verify_runtime_binary(rootfs: &Path, guest: &'static str) -> Result<(), BaseImageError> {
    let Some(path) = resolve_guest_path(rootfs, guest) else {
        return Err(BaseImageError::MissingRuntimeBinary { guest_path: guest });
    };
    let metadata = std::fs::metadata(&path).map_err(|error| BaseImageError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(BaseImageError::MissingRuntimeBinary { guest_path: guest });
    }
    Ok(())
}

fn verify_crun_digest(rootfs: &Path, pin: &CrunPin) -> Result<(), BaseImageError> {
    let Some(path) = resolve_guest_path(rootfs, CRUN_GUEST_PATH) else {
        return Err(BaseImageError::MissingRuntimeBinary {
            guest_path: CRUN_GUEST_PATH,
        });
    };
    let bytes = std::fs::read(&path).map_err(|error| BaseImageError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let actual = sha256_hex(&bytes);
    if actual != pin.sha256 {
        return Err(BaseImageError::ExtractedCrunMismatch {
            guest_path: CRUN_GUEST_PATH,
            expected: pin.sha256.to_owned(),
            actual,
            release_url: pin.release_url,
        });
    }
    Ok(())
}

fn verify_world_init_digest(rootfs: &Path, runtime: &RuntimeAssets) -> Result<(), BaseImageError> {
    let expected = std::fs::read(&runtime.world_init).map_err(|error| BaseImageError::Io {
        path: runtime.world_init.display().to_string(),
        message: error.to_string(),
    })?;
    let expected = sha256_hex(&expected);
    let Some(path) = resolve_guest_path(rootfs, WORLD_INIT_GUEST_PATH) else {
        return Err(BaseImageError::MissingRuntimeBinary {
            guest_path: WORLD_INIT_GUEST_PATH,
        });
    };
    let actual = std::fs::read(&path).map_err(|error| BaseImageError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let actual = sha256_hex(&actual);
    if actual != expected {
        return Err(BaseImageError::Io {
            path: WORLD_INIT_GUEST_PATH.to_owned(),
            message: format!(
                "extracted sha256 {actual} does not match world init sha256 {expected}"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{healthcheck_ports, is_supported, parse_os_release};

    #[test]
    fn supported_distro_table() {
        for (raw, expected) in [
            ("ID=debian\nVERSION_CODENAME=trixie\n", true),
            ("ID=debian\nVERSION_ID=12\n", true),
            ("ID=ubuntu\nVERSION_CODENAME=noble\n", true),
            ("ID=alpine\nVERSION_ID=3.21\n", true),
            ("ID=fedora\nVERSION_ID=41\n", false),
            ("ID=alpine\nVERSION_ID=3.18\n", false),
            ("ID=distroless\n", false),
        ] {
            let parsed = parse_os_release(raw.as_bytes()).expect("parse");
            assert_eq!(is_supported(&parsed), expected, "{raw}");
        }
    }

    #[test]
    fn healthcheck_ports_finds_url_and_loopback_ports() {
        assert_eq!(
            healthcheck_ports("CMD curl -fsS http://localhost:3000/healthz || exit 1"),
            [3000]
        );
        assert_eq!(
            healthcheck_ports("CMD-SHELL curl http://127.0.0.1:8000/"),
            [8000]
        );
    }
}
