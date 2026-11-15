//! Shared helpers for the `slipfork-engine` rootfs critical tests.
//!
//! The pinned static `crun` asset is a real input: it is downloaded once from the URL recorded in
//! `rootfs::pin` and verified against the compile-time SHA-256. Nothing here mocks Docker,
//! filesystems, or the pin.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use slipfork_engine::rootfs::{
    BaseImageBuilder, BaseImageError, CliBuildxExporter, DockerfileStageBuilder, ElfAbiProbe,
    FsInputHasher, HostCacheRequest, OciImageStore, OciRootfs, RuntimeAssets, SnowPortContract,
    WorldRootfs, WorldRootfsRequest,
};

/// Compile-time pin mirrored for the tests (the crate's `pin` module is the source of truth).
pub const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
/// Compile-time SHA-256 of the pinned static `crun`.
pub const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
/// Release asset the pin was measured from (`docs/design/world-base-image.md` §7.1).
pub const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";

/// Local cache holding the pinned `crun` between test runs.
pub fn runtime_cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime")
}

/// Disk-backed scratch root (`/tmp` is RAM-backed `tmpfs` on Linux runners).
pub fn disk_scratch_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-tmp")
}

/// A temporary directory on disk, never on `tmpfs`.
pub fn disk_tempdir(label: &str) -> tempfile::TempDir {
    let root = disk_scratch_root();
    std::fs::create_dir_all(&root).expect("create disk scratch root");
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in(root)
        .expect("disk tempdir")
}

/// Path of the pinned static `crun`, downloading and verifying it on a cold cache.
pub fn pinned_crun() -> PathBuf {
    let path = runtime_cache_dir().join(CRUN_FILE_NAME);
    if verify_sha256(&path) {
        return path;
    }
    let cache = runtime_cache_dir();
    std::fs::create_dir_all(&cache).expect("create runtime cache");
    let temporary = cache.join(format!("{CRUN_FILE_NAME}.download"));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(CRUN_URL)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to execute curl to fetch the pinned static crun: {error}\n  fix: install curl, or place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
                cache.display()
            )
        });
    assert!(
        status.success(),
        "failed to download the pinned static crun from {CRUN_URL}\n  fix: place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
        cache.display()
    );
    assert!(
        verify_sha256(&temporary),
        "downloaded crun does not match the compile-time pin {CRUN_SHA256}"
    );
    std::fs::rename(&temporary, &path).expect("install pinned crun");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make crun executable");
    path
}

fn verify_sha256(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes)) == CRUN_SHA256
}

/// Test fixture `Dockerfile`: a real supported `L0` stage plus the client tools the protected
/// engine scripts exercise (`bash` for `/dev/tcp`, coreutils for `stat`/`wc`, Python for sockets).
pub const FIXTURE_DOCKERFILE: &str = "\
FROM alpine:3.21 AS app
RUN apk add --no-cache bash coreutils python3
WORKDIR /workspace
";

/// Marker file proving a previously materialized fixture is complete.
const FIXTURE_MARKER: &str = ".slipfork/bin/slipfork-world-init";

struct NoServices;

impl OciImageStore for NoServices {
    fn materialize(
        &self,
        image: &slipfork_engine::BackingServiceKey,
    ) -> Result<OciRootfs, BaseImageError> {
        panic!("the engine test fixture requests no backing services, got {image}");
    }
}

/// A real [`WorldRootfs`] materialized through the production [`DockerfileStageBuilder`].
///
/// The Dockerfile is a genuine `alpine:3.21` stage; the pinned static `crun` and
/// `slipfork-world-init` enter through the same `slipfork-runtime` build context used by
/// production. Nothing here shares or hardlinks the host rootfs. A missing `docker`/`buildx`
/// fails closed with the ADR-0009 §5 diagnostic — the tests never skip.
pub fn fixture_rootfs() -> WorldRootfs {
    static ROOTFS: OnceLock<WorldRootfs> = OnceLock::new();
    ROOTFS
        .get_or_init(|| {
            let dir = disk_scratch_root().join("engine-fixture");
            let context = dir.join("context");
            std::fs::create_dir_all(&context).expect("create fixture context");
            std::fs::write(context.join("Dockerfile"), FIXTURE_DOCKERFILE)
                .expect("write fixture Dockerfile");
            let world_init = PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init"));
            assert!(
                world_init.is_file(),
                "cargo must build slipfork-world-init for the protected engine tests: {}",
                world_init.display()
            );
            let request = WorldRootfsRequest {
                state_dir: dir.join("state"),
                image_id: "fixture".to_owned(),
                context_dir: context.clone(),
                context_label: ".".to_owned(),
                dockerfile: context.join("Dockerfile"),
                target_stage: None,
                build_envs: Vec::new(),
                target_arch: "x86_64".to_owned(),
                services: Vec::new(),
                ports: SnowPortContract {
                    component: "fixture".to_owned(),
                    internal_ports: Vec::new(),
                    ingress_ports: Vec::new(),
                },
                uid: current_uid(),
                gid: current_gid(),
                runtime: RuntimeAssets {
                    crun: pinned_crun(),
                    world_init,
                },
                host_caches: HostCacheRequest::default(),
            };
            let builder = DockerfileStageBuilder::new(
                std::sync::Arc::new(CliBuildxExporter::new("docker")),
                std::sync::Arc::new(FsInputHasher),
                std::sync::Arc::new(NoServices),
                std::sync::Arc::new(ElfAbiProbe),
            );
            let mut events = Vec::new();
            let rootfs = builder
                .materialize(&request, &mut |event| events.push(event))
                .unwrap_or_else(|error| {
                    panic!(
                        "the protected engine fixture rootfs could not be materialized:\n{error}"
                    )
                });
            assert!(
                events.last().is_some(),
                "materialization must report progress: {events:?}"
            );
            assert!(
                rootfs.app_rootfs.join(FIXTURE_MARKER).is_file(),
                "materialized fixture must carry the static world init: {}",
                rootfs.app_rootfs.join(FIXTURE_MARKER).display()
            );
            rootfs
        })
        .clone()
}

fn current_uid() -> u32 {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|raw| raw.trim().parse().ok())
        .expect("resolve the invoking uid")
}

fn current_gid() -> u32 {
    Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|raw| raw.trim().parse().ok())
        .expect("resolve the invoking gid")
}

/// Materialize one digest-pinned OCI image into the content-addressed test cache.
///
/// `extra_paths` are prepared **inside** the extracted rootfs so `crun` can bind a state or
/// read-only mount at an existing destination (the runtime never mutates a read-only rootfs).
/// A missing Docker daemon or a failed pull fails the test with the exact fix, never a skip.
pub fn oci_rootfs(image_ref: &str, extra_paths: &[&str]) -> OciRootfs {
    let digest = resolve_image_digest(image_ref);
    let hex = digest
        .strip_prefix("sha256:")
        .expect("RepoDigest must be sha256:<hex>");
    let cache = disk_scratch_root().join("oci").join(hex);
    let rootfs = cache.join("rootfs");
    if !rootfs.is_dir() || !cache.join("config.json").is_file() {
        let staging = cache.with_extension(format!("staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).expect("create OCI staging rootfs");
        let container = Command::new("docker")
            .args(["create", image_ref])
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "docker is required to materialize {image_ref}: {error}\n  fix: install Docker and verify with `docker version`"
                )
            });
        assert!(
            container.status.success(),
            "docker create {image_ref} failed: {}",
            String::from_utf8_lossy(&container.stderr)
        );
        let id = String::from_utf8_lossy(&container.stdout).trim().to_owned();
        let tar = staging.with_extension("tar");
        let export = Command::new("docker")
            .args(["export", "-o"])
            .arg(&tar)
            .arg(&id)
            .status()
            .expect("docker export");
        let _ = Command::new("docker").args(["rm", "-f", &id]).status();
        assert!(
            export.success(),
            "docker export {image_ref} ({id}) failed with {export}\n  fix: verify the image is pullable and Docker is healthy"
        );
        let status = Command::new("tar")
            .args(["-xf"])
            .arg(&tar)
            .arg("-C")
            .arg(&staging)
            .status()
            .expect("extract OCI rootfs");
        assert!(status.success(), "tar extraction of {image_ref} failed");
        let _ = std::fs::remove_file(&tar);
        std::fs::create_dir_all(&cache).expect("create OCI cache");
        let _ = std::fs::remove_dir_all(&rootfs);
        std::fs::rename(&staging, &rootfs).expect("install OCI rootfs");
        std::fs::write(cache.join("config.json"), b"{}").expect("write OCI config");
    }
    for path in extra_paths {
        let target = rootfs.join(path.trim_start_matches('/'));
        if path.ends_with('/') {
            std::fs::create_dir_all(&target).unwrap_or_else(|error| {
                panic!("create {} in {image_ref}: {error}", target.display())
            });
        } else {
            let parent = target.parent().expect("destination parent");
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create {}: {error}", parent.display()));
            if !target.exists() {
                std::fs::write(&target, b"").expect("create read-only mount destination");
            }
        }
    }
    OciRootfs {
        digest,
        rootfs,
        config: cache.join("config.json"),
    }
}

fn resolve_image_digest(image_ref: &str) -> String {
    let inspect = || {
        Command::new("docker")
            .args([
                "image",
                "inspect",
                image_ref,
                "--format",
                "{{index .RepoDigests 0}}",
            ])
            .output()
    };
    let mut output = inspect().unwrap_or_else(|error| {
        panic!(
            "docker is required to resolve {image_ref}: {error}\n  fix: install Docker and verify with `docker version`"
        )
    });
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        let pull = Command::new("docker")
            .args(["pull", "-q", image_ref])
            .status()
            .unwrap_or_else(|error| {
                panic!(
                    "cannot pull {image_ref}: {error}\n  fix: verify network access to docker.io"
                )
            });
        assert!(
            pull.success(),
            "docker pull {image_ref} failed with {pull}\n  fix: verify network access to docker.io"
        );
        output = inspect().expect("docker image inspect after pull");
    }
    assert!(
        output.status.success(),
        "docker image inspect {image_ref} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo_digest = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    repo_digest
        .rsplit_once('@')
        .map(|(_, digest)| digest.to_owned())
        .filter(|digest| {
            digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .unwrap_or_else(|| {
            panic!(
                "{image_ref} resolves to '{repo_digest}', not a digest-pinned sha256 reference\n  fix: pull the official digest-pinned image"
            )
        })
}

/// Decode the base64 text streamed out of a container (`base64 -w0`).
pub fn decode_base64(raw: &str) -> Vec<u8> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0;
    for byte in raw.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        let Some(value) = value(byte) else { continue };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    out
}

/// Recursively copy `from` into `to`, preserving Unix modes.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            copy_tree(&source, &target)?;
        } else if metadata.file_type().is_symlink() {
            let link = std::fs::read_link(&source)?;
            std::os::unix::fs::symlink(link, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}
