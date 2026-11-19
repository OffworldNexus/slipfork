//! `L0` world root filesystem materialization (`ADR-0009` §1–§3).
//!
//! One mechanism only, zero fallbacks: every [`WorldRootfs`] is extracted from the project's own
//! `snow.yml` `images:` `Dockerfile` (using its named target stage), validated against the
//! 6-rule World Image Contract, and cached content-addressed under
//! `<state_dir>/cache/{rootfs,oci,runtime}` on non-`tmpfs` disk. Slipfork never curates language
//! base images, never shares the host `/usr`, and never installs backing services into the app
//! stage.
//!
//! The module is engine-agnostic and dependency-inverted: [`DockerfileStageBuilder`] is the only
//! production [`BaseImageBuilder`] and receives every host capability — `docker buildx`
//! ([`DockerBuildx`]), build-context hashing ([`FsInputHasher`]), backing-service rootfs
//! extraction ([`DockerImageStore`]), and ELF/libc inspection ([`ElfProbe`]) — through its
//! constructor. The builder spawns nothing: [`DockerBuildx`] and [`DockerImageStore`] own the
//! Docker CLI, and no `std::env` or ambient host probe is touched here.
//!
//! Every contract violation halts before a partial world directory exists and renders the exact
//! [`ADR-0009`] §5 user-facing diagnostic, including its `  fix:` line. Callers print
//! [`BaseImageError`] verbatim (its `Display` already carries the `error:` prefix).
//!
//! [`ADR-0009`]: docs/decisions/ADR-0009-world-base-image-and-image-contract.md

#![forbid(unsafe_code)]

pub mod abi;
pub mod builder;
pub mod buildx;
pub(crate) mod cache;
pub(crate) mod contract;
pub mod dedupe;
pub(crate) mod dockerfile;
pub mod input;
pub(crate) mod oci_store;
pub mod pin;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub use abi::ElfProbe;
pub use builder::DockerfileStageBuilder;
pub use buildx::{BuildxRequest, DockerBuildx};
pub use dedupe::{BuildSource, PodImageSource, RootfsEntry, RootfsKey, RootfsPlan};
pub use input::FsInputHasher;
pub use oci_store::DockerImageStore;

use slipfork_world::WorldMount;
use slipfork_world::rootfs::{
    BackingServiceKey, BaseImageError, BuildEnvVar, RootfsDigest, WorldRootfs,
};

/// Guest path of the static `crun` binary every world rootfs carries (`ADR-0009` §1).
pub const CRUN_GUEST_PATH: &str = "/.slipfork/bin/crun";

/// Guest path of the world PID 1 helper every world rootfs carries (`ADR-0009` §1).
pub const WORLD_INIT_GUEST_PATH: &str = "/.slipfork/bin/slipfork-world-init";

/// `snow.yml` ports of the component that uses the selected image (World Image Contract #6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnowPortContract {
    /// Component id the diagnostic names (`api`).
    pub component: String,
    /// `components[].internal_ports`.
    pub internal_ports: Vec<u16>,
    /// `ingresses[].target.port` of that component.
    pub ingress_ports: Vec<u16>,
}

impl SnowPortContract {
    /// Every port `snow.yml` allows the image to serve.
    pub fn allowed_ports(&self) -> BTreeSet<u16> {
        self.internal_ports
            .iter()
            .chain(self.ingress_ports.iter())
            .copied()
            .collect()
    }
}

/// Host binaries staged into `/.slipfork/bin` inside the built world rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAssets {
    /// Static `crun` binary on the host; its SHA-256 must equal [`pin::crun_pin`].
    pub crun: PathBuf,
    /// `slipfork-world-init` helper on the host.
    pub world_init: PathBuf,
}

/// Opt-in `--host-caches` request (`ADR-0009` §2.3).
///
/// Empty [`Self::mounts`] means "no host caches": no ABI probe runs and
/// [`WorldRootfs::host_caches`] is empty. Any non-empty request probes `host_shell` against the
/// world `/bin/sh` and fails closed on an architecture/libc mismatch — never a silent skip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCacheRequest {
    /// Host ELF used as the ABI witness (`/bin/sh`), injected by the Composition Root.
    pub host_shell: PathBuf,
    /// Read-only host cache mounts to attach once the ABI probe passes.
    pub mounts: Vec<WorldMount>,
}

/// Everything one [`BaseImageBuilder`] needs; resolved by `slipforkd` from `snow.yml`/`SlipforkConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRootfsRequest {
    /// Slipfork state directory; the cache lives in `<state_dir>/cache`.
    pub state_dir: PathBuf,
    /// Selected `snow.yml` `images[].id`.
    pub image_id: String,
    /// Absolute build context directory.
    pub context_dir: PathBuf,
    /// `snow.yml` `images[].context`, as declared (digest input).
    pub context_label: String,
    /// Absolute path to the selected `Dockerfile`.
    pub dockerfile: PathBuf,
    /// Requested target stage; `None` targets the final stage, which must then be named.
    pub target_stage: Option<String>,
    /// Resolved `snow.yml` `images[].envs`.
    pub build_envs: Vec<BuildEnvVar>,
    /// Target platform (`x86_64`, `aarch64`).
    pub target_arch: String,
    /// Backing services resolved from `snow.yml` `resources:`.
    pub services: Vec<BackingServiceKey>,
    /// Ports `snow.yml` declares for this image's component.
    pub ports: SnowPortContract,
    /// Invoking developer's uid (used in the World Image Contract #3 diagnostic).
    pub uid: u32,
    /// Invoking developer's gid (used in the World Image Contract #3 diagnostic).
    pub gid: u32,
    /// Host runtime binaries staged into `/.slipfork/bin`.
    pub runtime: RuntimeAssets,
    /// Opt-in host-cache passthrough.
    pub host_caches: HostCacheRequest,
}

/// Progress emitted while a [`BaseImageBuilder`] materializes a rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootfsBuildEvent {
    /// The digest was already installed; no build ran.
    CacheHit { digest: RootfsDigest },
    /// The digest must be built.
    CacheMiss { digest: RootfsDigest },
    /// Extracted tree validated against the 6-rule World Image Contract.
    ContractValidated { digest: RootfsDigest },
    /// One backing-service OCI rootfs is being extracted.
    MaterializingService { image: BackingServiceKey },
    /// One long rootfs step emitted a line (`[step/total] phase …`); `detail` is live child output.
    Progress {
        /// 1-based step inside the rootfs pipeline.
        step: u8,
        /// Number of steps in the rootfs pipeline.
        total: u8,
        /// Human phase name (`docker buildx build`).
        phase: String,
        /// Latest child output line, when the step streams one.
        detail: Option<String>,
    },
    /// The rootfs is installed and addressable by `digest`.
    Installed {
        digest: RootfsDigest,
        app_rootfs: PathBuf,
    },
}

/// Single production port for materializing a world rootfs (`ADR-0009` §1–§3).
pub trait BaseImageBuilder: Send + Sync {
    /// Build (or reuse) and validate the rootfs described by `req`, reporting progress.
    fn materialize(
        &self,
        req: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<WorldRootfs, BaseImageError>;
}

/// Resolve `guest` inside `rootfs` without ever escaping the tree (`WorldRootfs::guest_binary`).
pub fn guest_binary(rootfs: &WorldRootfs, guest: &str) -> Option<PathBuf> {
    resolve_guest_path(&rootfs.app_rootfs, guest)
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Resolve `guest` inside `rootfs`, following symlinks without ever escaping the tree.
///
/// Extracted stages index files through symlinks (`/bin/sh -> /bin/busybox`, `/lib` ->
/// `/usr/lib`), so resolving them is mandatory. Absolute symlink targets restart at `rootfs`
/// (never the host `/`), `..` may not climb above it, and the hop count is bounded — a symlink
/// that leaves the tree or loops is treated as absent (fail closed).
pub(crate) fn resolve_guest_path(rootfs: &Path, guest: &str) -> Option<PathBuf> {
    use std::collections::VecDeque;

    const MAX_SYMLINK_HOPS: usize = 40;

    let root = std::fs::canonicalize(rootfs).ok()?;
    let mut current = root.clone();
    let mut pending: VecDeque<String> = guest
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    let mut hops = 0;

    while let Some(component) = pending.pop_front() {
        match component.as_str() {
            "." => continue,
            ".." => {
                current.pop();
                if !current.starts_with(&root) {
                    return None;
                }
                continue;
            }
            _ => {}
        }
        let candidate = current.join(&component);
        let metadata = std::fs::symlink_metadata(&candidate).ok()?;
        if metadata.file_type().is_symlink() {
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return None;
            }
            let target = std::fs::read_link(&candidate).ok()?;
            let target = target.to_string_lossy();
            if target.starts_with('/') {
                current = root.clone();
            }
            for segment in target
                .split('/')
                .rev()
                .filter(|segment| !segment.is_empty())
            {
                pending.push_front(segment.to_owned());
            }
            continue;
        }
        current = candidate;
    }
    current.starts_with(&root).then_some(current)
}

/// Read a file inside `rootfs` without following a symlink out of it.
pub(crate) fn read_guest_file(rootfs: &Path, guest: &str) -> Option<Vec<u8>> {
    let path = resolve_guest_path(rootfs, guest)?;
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use slipfork_world::rootfs::{BuildEnvVar, RootfsDigest, RootfsDigestInputs};

    fn inputs<'a>(envs: &'a [BuildEnvVar]) -> RootfsDigestInputs<'a> {
        RootfsDigestInputs {
            image_id: "api",
            dockerfile_rel_path: "Dockerfile",
            dockerfile_bytes: b"FROM alpine:3.21 AS app\n",
            context_rel_path: ".",
            context_merkle_sha256: "00",
            target_stage: "app",
            build_envs: envs,
            crun_binary_sha256: "11",
            world_init_sha256: "22",
            target_arch: "x86_64",
        }
    }

    #[test]
    fn digest_is_stable_and_hex_shaped() {
        let digest = RootfsDigest::compute(&inputs(&[]));
        assert_eq!(digest.hex().len(), 64);
        assert_eq!(digest.hex(), digest.hex().to_lowercase());
        assert_eq!(
            RootfsDigest::compute(&inputs(&[])).hex(),
            digest.hex(),
            "same inputs must produce the same Merkle key"
        );
        assert_eq!(digest.short(), &digest.hex()[..12]);
    }

    #[test]
    fn digest_hashes_secret_env_values_without_storing_them() {
        let envs = [BuildEnvVar {
            name: "NPM_TOKEN".to_owned(),
            value: "super-secret".to_owned(),
        }];
        let changed = RootfsDigest::compute(&inputs(&envs));
        let plain = RootfsDigest::compute(&inputs(&[]));
        assert_ne!(changed.hex(), plain.hex());
        assert!(!format!("{changed:?}").contains("super-secret"));
        assert!(!format!("{:?}", envs[0]).contains("super-secret"));
    }

    #[test]
    fn digest_rejects_non_hex() {
        assert!(RootfsDigest::from_hex("ABC").is_err());
    }

    #[test]
    fn error_stays_small_enough_for_result() {
        assert!(
            std::mem::size_of::<super::BaseImageError>() <= 128,
            "BaseImageError is {} bytes; box large payloads to keep `Result` cheap",
            std::mem::size_of::<super::BaseImageError>()
        );
    }
}
