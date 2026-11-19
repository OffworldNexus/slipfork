//! The real pinned `crun` asset and the real `alpine:3.21` `L0` fixture rootfs.
//!
//! Both are materialized exactly the way production materializes them: the static `crun` is
//! downloaded once from the URL recorded in `rootfs::pin` and verified against the compile-time
//! SHA-256, and the `L0` tree is a genuine `docker buildx` extraction through
//! [`DockerfileStageBuilder`]. Nothing here mocks Docker, filesystems, or the pin.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use slipfork_engine::current_uid_gid;
use slipfork_engine::rootfs::{
    BaseImageBuilder, DockerBuildx, DockerImageStore, DockerfileStageBuilder, ElfProbe,
    FsInputHasher, HostCacheRequest, RuntimeAssets, SnowPortContract, WorldRootfsRequest,
};
use slipfork_testkit::live::disk_scratch_root;
use slipfork_world::rootfs::WorldRootfs;

/// Compile-time pin mirrored for the tests (the crate's `pin` module is the source of truth).
const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
/// Compile-time SHA-256 of the pinned static `crun`.
const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
/// Release asset the pin was measured from (`docs/design/world-base-image.md` §7.1).
const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";

/// Local cache holding the pinned `crun` between test runs.
fn runtime_cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime")
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
const FIXTURE_DOCKERFILE: &str = "\
FROM alpine:3.21 AS app
RUN apk add --no-cache bash coreutils python3
WORKDIR /workspace
";

/// Marker file proving a previously materialized fixture is complete.
const FIXTURE_MARKER: &str = ".slipfork/bin/slipfork-world-init";

/// A real [`WorldRootfs`] materialized through the production [`DockerfileStageBuilder`].
///
/// The Dockerfile is a genuine `alpine:3.21` stage; the pinned static `crun` and
/// `slipfork-world-init` enter through the same `slipfork-runtime` build context used by
/// production. Nothing here shares or hardlinks the host rootfs. A missing `docker`/`buildx`
/// fails closed with the ADR-0009 §5 diagnostic — the tests never skip.
fn fixture_rootfs() -> WorldRootfs {
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
            let (uid, gid) = current_uid_gid();
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
                uid,
                gid,
                runtime: RuntimeAssets {
                    crun: pinned_crun(),
                    world_init,
                },
                host_caches: HostCacheRequest::default(),
            };
            let builder = DockerfileStageBuilder::new(
                DockerBuildx::new("docker"),
                FsInputHasher,
                DockerImageStore::new(dir.join("state")),
                ElfProbe,
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

/// The engine-valid [`WorldRootfs`] for `state_dir`, seeded from the shared fixture cache.
///
/// [`BtrfsEngine`](slipfork_engine::BtrfsEngine) only accepts a rootfs whose `app_rootfs` is the
/// content-addressed `<state_dir>/cache/rootfs/<digest>/app` tree it owns
/// (`WorldEngine::clone_world`), while [`fixture_rootfs`] materializes once per test run into one
/// shared cache. The real, immutable `L0` tree is therefore hardlinked into each suite's own state
/// directory: byte-identical content, the production path layout, and no second Docker build.
/// Overlayfs only ever reads a lowerdir, so the shared inodes are never written through.
pub fn engine_rootfs(state_dir: &Path) -> WorldRootfs {
    let fixture = fixture_rootfs();
    let app_rootfs = state_dir
        .join("cache")
        .join("rootfs")
        .join(fixture.digest.hex())
        .join("app");
    if !app_rootfs.is_dir() {
        link_tree(&fixture.app_rootfs, &app_rootfs).unwrap_or_else(|error| {
            panic!(
                "seed the content-addressed rootfs '{}' from '{}': {error}",
                app_rootfs.display(),
                fixture.app_rootfs.display()
            )
        });
    }
    WorldRootfs {
        app_rootfs,
        ..fixture
    }
}

/// Recreate `from` at `to` with directories and symlinks copied and regular files hardlinked.
fn link_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            link_tree(&source, &target)?;
        } else if metadata.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else {
            // A hardlink shares the fixture's inode (and mode): never chmod it here.
            std::fs::hard_link(&source, &target)?;
        }
    }
    // Post-order: a mode-0555 directory (the image's `/proc`, `/var/empty`) is applied only after
    // its children exist.
    std::fs::set_permissions(to, std::fs::metadata(from)?.permissions())
}
