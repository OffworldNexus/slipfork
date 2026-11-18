//! Pure `L0` world rootfs values: content-addressed digest, extracted-tree provenance,
//! backing-service image keys, `--host-caches` ABI facts, and the contract-validated image view.
//!
//! Materialization (Docker build, extraction, validation) lives in `slipfork-engine`; this module
//! owns only the plain values every engine, edge proxy, and test suite shares. No method here
//! spawns a process, reads the ambient environment, or formats a CLI table.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CacheOutcome, WorldMount};

/// SHA-256 prefix of the `L0` rootfs Merkle key (`ADR-0009` §3).
const ROOTFS_DIGEST_DOMAIN: &[u8] = b"slipfork-l0-rootfs:v1\0";

/// Content-addressed identifier of one extracted `L0` app rootfs (`ADR-0009` §3).
///
/// Rendered as `sha256:<64 lowercase hex>`; the cache directory uses [`Self::hex`] alone.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootfsDigest(String);

impl RootfsDigest {
    /// Wrap an already-computed lowercase hex SHA-256.
    pub fn from_hex(hex: impl Into<String>) -> Result<Self, BaseImageError> {
        let hex = hex.into();
        if hex.len() != 64
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
            || hex != hex.to_lowercase()
        {
            return Err(BaseImageError::InvalidDigest { digest: hex });
        }
        Ok(Self(hex))
    }

    /// Bare lowercase hex, used as the cache directory name.
    pub fn hex(&self) -> &str {
        &self.0
    }

    /// First 12 hex characters of the digest — a compact label for diagnostics; the cache
    /// directory itself is named by the full [`Self::hex`] (`ADR-0009` §3).
    pub fn short(&self) -> &str {
        &self.0[..12]
    }

    /// Compute the `ADR-0009` §3 Merkle key over the exact listed components.
    pub fn compute(inputs: &RootfsDigestInputs<'_>) -> Self {
        let mut hash = Sha256::new();
        hash.update(ROOTFS_DIGEST_DOMAIN);
        let mut push = |bytes: &[u8]| {
            hash.update(bytes);
            hash.update(b"\0");
        };
        push(inputs.image_id.as_bytes());
        push(inputs.dockerfile_rel_path.as_bytes());
        push(sha256_hex(inputs.dockerfile_bytes).as_bytes());
        push(inputs.context_rel_path.as_bytes());
        push(inputs.context_merkle_sha256.as_bytes());
        push(inputs.target_stage.as_bytes());
        let mut envs: Vec<&BuildEnvVar> = inputs.build_envs.iter().collect();
        envs.sort_by(|a, b| a.name.cmp(&b.name));
        for env in envs {
            push(env.name.as_bytes());
            push(sha256_hex(env.value.as_bytes()).as_bytes());
        }
        push(inputs.crun_binary_sha256.as_bytes());
        push(inputs.world_init_sha256.as_bytes());
        push(inputs.target_arch.as_bytes());
        Self(hex::encode(hash.finalize()))
    }
}

impl fmt::Display for RootfsDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.0)
    }
}

impl fmt::Debug for RootfsDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RootfsDigest(sha256:{})", self.0)
    }
}

/// Every component of the [`RootfsDigest`] formula, in `ADR-0009` §3 order.
#[derive(Debug, Clone, Copy)]
pub struct RootfsDigestInputs<'a> {
    /// `snow.yml` `images[].id`.
    pub image_id: &'a str,
    /// `Dockerfile` path relative to its build context, as declared.
    pub dockerfile_rel_path: &'a str,
    /// Raw `Dockerfile` bytes (never rewritten in the worktree).
    pub dockerfile_bytes: &'a [u8],
    /// `snow.yml` `images[].context`, as declared.
    pub context_rel_path: &'a str,
    /// `.dockerignore`-filtered context Merkle SHA-256 (`InputHasher`).
    pub context_merkle_sha256: &'a str,
    /// Selected `Dockerfile` stage name.
    pub target_stage: &'a str,
    /// Resolved `snow.yml` `images[].envs` build arguments.
    pub build_envs: &'a [BuildEnvVar],
    /// SHA-256 of the pinned static `crun` binary.
    pub crun_binary_sha256: &'a str,
    /// SHA-256 of the `slipfork-world-init` helper.
    pub world_init_sha256: &'a str,
    /// Target platform (`x86_64`, `aarch64`).
    pub target_arch: &'a str,
}

/// One resolved `snow.yml` `images[].envs` build argument.
///
/// The value is usually a secret: its [`fmt::Debug`] never renders it, and the [`RootfsDigest`]
/// formula only hashes it (`ADR-0009` §3).
#[derive(Clone, PartialEq, Eq)]
pub struct BuildEnvVar {
    /// Build-argument name (`FROM`/`ARG` variable).
    pub name: String,
    /// Resolved value (hashed, never stored in a cache key).
    pub value: String,
}

impl fmt::Debug for BuildEnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildEnvVar")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// One backing-service OCI rootfs extracted into `<state_dir>/cache/oci/<sha256>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciRootfs {
    /// Upstream image digest (`sha256:<64 lowercase hex>`) the rootfs was extracted from.
    pub digest: String,
    /// Extracted `$UID:$GID`-owned rootfs, mounted read-only into `crun`.
    pub rootfs: PathBuf,
    /// Upstream image `config.json` (`ENV`/`PATH` metadata).
    pub config: PathBuf,
    /// Cache outcome the `DockerImageStore` observed for this digest.
    pub cache: CacheOutcome,
    /// Measured export/extract milliseconds (`None` when the image was already cached).
    pub millis: Option<u64>,
}

/// Stable identity of one backing service requested by `snow.yml` `resources:`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackingServiceKey {
    /// `resources.pg_clusters[].version` -> `docker.io/library/postgres:<version>`.
    Postgres {
        /// Declared cluster major version (`"18"`).
        version: String,
    },
    /// `resources.redis_clusters[].version` -> `docker.io/library/redis:<version>`.
    Redis {
        /// Declared cluster major version (`"8"`).
        version: String,
    },
    /// Non-empty `resources.s3_buckets` -> digest-pinned SeaweedFS.
    SeaweedFs,
}

impl BackingServiceKey {
    /// Official repository or digest-pinned reference (`ADR-0009` §2.2).
    ///
    /// SeaweedFS carries no tag here because its digest pin is part of the
    /// `DockerImageStore` adapter's registry resolution, never a floating tag.
    pub fn image_repository(&self) -> &'static str {
        match self {
            Self::Postgres { .. } => "docker.io/library/postgres",
            Self::Redis { .. } => "docker.io/library/redis",
            Self::SeaweedFs => "docker.io/chrislusf/seaweedfs",
        }
    }

    /// Declared major version, absent for the digest-pinned SeaweedFS image.
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Postgres { version } | Self::Redis { version } => Some(version),
            Self::SeaweedFs => None,
        }
    }
}

impl fmt::Display for BackingServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres { version } => write!(f, "postgres:{version}"),
            Self::Redis { version } => write!(f, "redis:{version}"),
            Self::SeaweedFs => f.write_str("seaweedfs"),
        }
    }
}

/// `Parsed /etc/os-release` of the extracted app stage (`ADR-0009` §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsRelease {
    /// `ID` (`debian`, `ubuntu`, `alpine`).
    pub id: String,
    /// `VERSION_ID` (`12`, `22.04`, `3.21`).
    pub version_id: String,
    /// `VERSION_CODENAME` (`trixie`, `jammy`), empty when absent.
    pub version_codename: String,
}

impl OsRelease {
    /// `debian:trixie`, `ubuntu:noble`, `alpine:3.21` — used in contract diagnostics.
    pub fn distro_label(&self) -> String {
        let release = if self.version_codename.is_empty() {
            if self.id == "alpine" {
                self.version_id
                    .split('.')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(".")
            } else {
                self.version_id.clone()
            }
        } else {
            self.version_codename.clone()
        };
        format!("{}:{release}", self.id)
    }
}

/// Verified facts about the extracted app stage, persisted in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContractMetadata {
    /// Linux distribution detected from `<app_rootfs>/etc/os-release`.
    pub os_release: OsRelease,
    /// ELF/libc ABI of `<app_rootfs>/bin/sh`.
    pub elf_abi: ElfAbi,
    /// Ports the image declares via `EXPOSE`/`HEALTHCHECK`.
    pub exposed_ports: BTreeSet<u16>,
}

/// Engine-agnostic, contract-validated `L0` rootfs handed to every [`crate::WorldEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRootfs {
    /// Content-addressed Merkle key (`ADR-0009` §3).
    pub digest: RootfsDigest,
    /// `<state_dir>/cache/rootfs/<hex>/app`, shared read-only by every world.
    pub app_rootfs: PathBuf,
    /// Guest path of the static `crun` binary (`CRUN_GUEST_PATH`).
    pub crun_bin: PathBuf,
    /// Backing-service OCI rootfs by `snow.yml` `resources:` key.
    pub service_images: BTreeMap<BackingServiceKey, OciRootfs>,
    /// Verified OS/ABI/port metadata.
    pub metadata: ImageContractMetadata,
    /// Read-only host caches, empty unless `--host-caches` was requested and the ABI probe passed.
    pub host_caches: Vec<WorldMount>,
    /// Cache outcome the builder observed for `digest` (`hit` served from cache, `miss` rebuilt).
    pub cache: CacheOutcome,
    /// Measured build milliseconds (`None` when the digest was already cached and no build ran).
    pub millis: Option<u64>,
}

/// C library family of one ELF binary (`ADR-0009` §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibcFamily {
    /// GNU C Library (`libc.so.6`).
    Glibc,
    /// musl libc (`ld-musl-*.so.1`).
    Musl,
}

impl fmt::Display for LibcFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Glibc => "glibc",
            Self::Musl => "musl",
        })
    }
}

/// Architecture + C library ABI of one ELF binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElfAbi {
    /// `e_machine` name (`x86_64`, `aarch64`).
    pub arch: String,
    /// C library family.
    pub libc: LibcFamily,
    /// `2.41` for glibc, `1.2` for musl.
    pub libc_version: String,
}

impl fmt::Display for ElfAbi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.arch, self.libc, self.libc_version)
    }
}

/// Fail-closed rootfs materialization errors (`ADR-0009` §5).
///
/// `Display` renders the complete user-facing diagnostic: the contract prefix, the exact
/// `  fix:` line, and no call-site decoration. Callers print the error verbatim.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BaseImageError {
    /// `ADR-0009` §5.1 — the request names no buildable image.
    #[error(
        "error: snow.yml declares no buildable Dockerfile under 'images:'; a project that cannot be built into an image is not a project Slipfork can fork\n  fix: add a named Dockerfile stage and declare it in snow.yml:\n    images:\n      - id: app\n        context: .\n        dockerfile: Dockerfile"
    )]
    NoBuildableImage,
    /// `ADR-0009` §5.4 — the selected final stage has no `AS <name>`.
    #[error(
        "error: World Image Contract #1 violated in Dockerfile:{line}: final stage `FROM {from}` has no stage name\n  fix: change Dockerfile:{line} to `FROM {from} AS app` (or set `world.stage = \"<stage>\"` in slipfork.toml)"
    )]
    UnnamedTargetStage {
        /// `Dockerfile` line of the unnamed final `FROM`.
        line: u32,
        /// Base image of that `FROM`.
        from: String,
    },
    /// `ADR-0009` §5.5 — `world.stage` names no declared stage.
    #[error(
        "error: World Image Contract #1 violated: `world.stage = \"{stage}\"` not found in Dockerfile (declared stages: {declared})\n  fix: set `world.stage = \"{suggested}\"` in slipfork.toml"
    )]
    UnknownTargetStage {
        /// Selected stage.
        stage: String,
        /// Declared stages, rendered as `'builder', 'runtime'`.
        declared: String,
        /// First declared stage.
        suggested: String,
    },
    /// `ADR-0009` §5.6 — unsupported distro or missing `/etc/os-release` + `/bin/sh`.
    #[error(
        "error: World Image Contract #2 violated in stage '{stage}' (Dockerfile:{line}): {detail}; supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)\n  fix: set `world.stage = \"builder\"` in slipfork.toml or switch `FROM` to a supported Debian, Ubuntu, or Alpine base"
    )]
    UnsupportedDistro {
        /// Selected stage.
        stage: String,
        /// `Dockerfile` line of the stage's `FROM`.
        line: u32,
        /// `detected distro 'fedora:41'` or `missing /etc/os-release + /bin/sh`.
        detail: String,
    },
    /// `ADR-0009` §5.7 — root-only privilege-drop entrypoint.
    #[error(
        "error: World Image Contract #3 violated in stage '{stage}': ENTRYPOINT {entrypoint:?} requires root to switch UIDs, but Slipfork always executes as host $UID:$GID ({uid}:{gid}) with a read-only '/' filesystem\n  fix: set an unprivileged command in snow.yml `modes.local.components.{component}.run_command` (or remove `gosu`/`su-exec`/`chown` from ENTRYPOINT)"
    )]
    RootAtRuntimeEntrypoint {
        /// Selected stage.
        stage: String,
        /// Offending argv.
        entrypoint: Vec<String>,
        /// Invoking uid.
        uid: u32,
        /// Invoking gid.
        gid: u32,
        /// Component whose `run_command` overrides the entrypoint.
        component: String,
    },
    /// `ADR-0009` §5.8 — baked origin/service URL or host path.
    #[error(
        "error: World Image Contract #4 violated in stage '{stage}': baked image {what} conflicts with Slipfork single-origin and ResourceCoordinates injection\n  fix: delete {what} from Dockerfile and reference `${{{{ resources::{component}.url }}}}` under `modes.all.envs` in snow.yml"
    )]
    BakedOriginOrHostPath {
        /// Selected stage.
        stage: String,
        /// `ENV `BASE_URL=...`` or `host path `/home/alice/app``.
        what: String,
        /// Component the fix points at.
        component: String,
    },
    /// `ADR-0009` §5.9 — backing-service daemon inside the app stage.
    #[error(
        "error: World Image Contract #5 violated in stage '{stage}': detected `{path}` (backing service daemon) installed inside the app image; Slipfork runs postgres, redis, and seaweedfs as isolated crun containers from snow.yml\n  fix: {fix}"
    )]
    EmbeddedBackingService {
        /// Selected stage.
        stage: String,
        /// Detected path inside the rootfs.
        path: String,
        /// Exact one-line fix for this daemon.
        fix: String,
    },
    /// `ADR-0009` §5.10 — `EXPOSE`/`HEALTHCHECK` disagrees with `snow.yml`.
    #[error(
        "error: World Image Contract #6 violated for image '{image}': Dockerfile {instruction} declares port {port}, but snow.yml component '{component}' declares internal_ports [{internal}] (ingresses target {ingress})\n  fix: change `EXPOSE {port}` to `EXPOSE {suggested}` in Dockerfile (or update `components.{component}.internal_ports` / `ingresses` in snow.yml to {port})"
    )]
    ExposeIngressMismatch {
        /// Selected `snow.yml` image id.
        image: Box<str>,
        /// `EXPOSE` or `HEALTHCHECK`.
        instruction: &'static str,
        /// Port the image declares.
        port: u16,
        /// Component id.
        component: Box<str>,
        /// Comma-joined `internal_ports`.
        internal: Box<str>,
        /// Comma-joined ingress target ports.
        ingress: Box<str>,
        /// Port the fix should declare.
        suggested: u16,
    },
    /// `ADR-0009` §5.11 — `--host-caches` ABI mismatch.
    #[error(
        "error: `--host-caches` ABI check failed: host is {host}, but world stage '{stage}' is {world} ({distro}); mounting host toolchains would fail to execute\n  fix: re-run without `--host-caches` (or use a matching {arch} {libc} >= {version} base such as `{suggested}`)"
    )]
    HostCacheAbiMismatch {
        /// Host ABI.
        host: Box<ElfAbi>,
        /// World ABI.
        world: Box<ElfAbi>,
        /// Selected stage.
        stage: Box<str>,
        /// Detected world distro.
        distro: Box<str>,
        /// Architecture the fix must match.
        arch: Box<str>,
        /// Libc family the fix must match.
        libc: LibcFamily,
        /// Host libc version the world must satisfy.
        version: Box<str>,
        /// Base image suggested by the fix.
        suggested: &'static str,
    },
    /// The request itself cannot be materialized (empty ids, relative paths, RW host caches).
    #[error("error: invalid WorldRootfsRequest: {reason}")]
    InvalidRequest {
        /// What is wrong and how to fix it.
        reason: String,
    },
    /// The cache would live on `tmpfs`, where extracted rootfs trees may never be stored.
    #[error(
        "error: Slipfork's rootfs cache root '{path}' is on tmpfs (mount '{mount_point}'); extracted world root filesystems must live on disk\n  fix: point --state-dir at a Btrfs filesystem mounted with -o user_subvol_rm_allowed,noatime — never /tmp or /dev/shm"
    )]
    CacheOnTmpfs {
        /// Cache root path.
        path: String,
        /// `tmpfs` mount point covering it.
        mount_point: String,
    },
    /// `docker buildx` is missing or refused the build.
    #[error(
        "error: Slipfork cannot build the L0 world image: {reason}\n  fix: install Docker with the buildx plugin and rerun (verify with `docker buildx version`)"
    )]
    Buildx {
        /// Actionable reason (missing binary, exit status + stderr tail).
        reason: String,
    },
    /// No static `crun` pin exists for the requested architecture.
    #[error(
        "error: no pinned static crun exists for architecture '{arch}'; Slipfork pins {pinned} only\n  fix: run Slipfork on a pinned architecture or add a verified pin for '{arch}'"
    )]
    UnsupportedArch {
        /// Requested architecture.
        arch: String,
        /// Architectures with a verified pin.
        pinned: &'static str,
    },
    /// ELF/libc inspection failed closed.
    #[error("error: cannot determine the ELF/libc ABI of {path}: {reason}")]
    ElfInspection {
        /// Binary or library path.
        path: String,
        /// Why the ABI is unknown.
        reason: String,
    },
    /// The extracted stage is missing a binary the world runtime requires.
    #[error(
        "error: the extracted slipfork-world stage is missing `{guest_path}`; Slipfork appends a `slipfork-world` stage that copies only `/.slipfork/bin/{{crun,slipfork-world-init}}`\n  fix: rerun the build; if it persists, verify that `docker buildx` exported a complete local tree"
    )]
    MissingRuntimeBinary {
        /// Guest path that is absent or not executable.
        guest_path: &'static str,
    },
    /// The extracted `crun` binary does not match the pin (COPY corruption or wrong asset).
    #[error(
        "error: the extracted `{guest_path}` has sha256 {actual}, but Slipfork pins {expected}\n  fix: rerun the build with the pinned static crun release asset ({release_url})"
    )]
    ExtractedCrunMismatch {
        /// Guest path.
        guest_path: &'static str,
        /// Compile-time pin.
        expected: String,
        /// Extracted digest.
        actual: String,
        /// Pinned release asset URL.
        release_url: &'static str,
    },
    /// Malformed `Dockerfile`.
    #[error("error: cannot parse Dockerfile: line {line}: {reason}")]
    DockerfileSyntax {
        /// 1-based line.
        line: u32,
        /// What is malformed.
        reason: String,
    },
    /// Backing-service rootfs extraction failed.
    #[error("error: cannot materialize backing-service image {image}: {reason}")]
    ServiceImage {
        /// Service key.
        image: String,
        /// Underlying failure.
        reason: String,
    },
    /// Cache filesystem failure.
    #[error("error: Slipfork rootfs cache I/O failed at {path}: {message}")]
    Io {
        /// Offending path.
        path: String,
        /// OS error message.
        message: String,
    },
    /// A digest was presented in a shape that is not `sha256:<64 lowercase hex>`.
    #[error("error: invalid rootfs digest '{digest}': expected 64 lowercase hex characters")]
    InvalidDigest {
        /// Offending digest.
        digest: String,
    },
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
