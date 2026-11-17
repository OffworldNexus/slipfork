//! Shared helpers for the `slipfork-engine` critical test suites.
//!
//! Every helper is real: the pinned static `crun` asset is downloaded once from the URL recorded
//! in `rootfs::pin` and verified against the compile-time SHA-256, the `L0` fixture is a genuine
//! `docker buildx` extraction, and the backing-service images are digest-pinned OCI trees. Nothing
//! here mocks Docker, filesystems, or the pin.
//!
//! Each integration-test binary compiles this module independently and links only the subset it
//! uses, so the suites declare `pub mod support;`: the shared surface stays crate-reachable while
//! a genuinely dead private helper still trips the `dead_code` lint.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use slipfork_engine::btrfs::SubvolumeDriver;
use slipfork_engine::rootfs::{
    BaseImageBuilder, BaseImageError, CliBuildxExporter, DockerfileStageBuilder, ElfAbiProbe,
    FsInputHasher, HostCacheRequest, OciImageStore, OciRootfs, RuntimeAssets, SnowPortContract,
    WorldRootfs, WorldRootfsRequest,
};
use slipfork_engine::{
    BtrfsEngine, ExecEnv, ExecOutput, InheritedVar, LocalIngressBridge, MountMode, ServiceHandle,
    ServiceLogs, ServiceSpec, SnapshotId, StateSubdirMount, StreamChunk, WorldEngine, WorldHandle,
    WorldMount, WorldSpec, current_uid_gid,
};
use slipfork_testkit::btrfs::TestTreeDriver;
use slipfork_world::WorldTarget;

/// Compile-time pin mirrored for the tests (the crate's `pin` module is the source of truth).
const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
/// Compile-time SHA-256 of the pinned static `crun`.
const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
/// Release asset the pin was measured from (`docs/design/world-base-image.md` §7.1).
const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";
/// Standard `PATH` used when the invoking shell exposes none.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// In-image postgres data directory used by every service suite.
pub const PG_DATA: &str = "/var/lib/postgresql/data";

/// Local cache holding the pinned `crun` between test runs.
fn runtime_cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime")
}

/// Disk-backed scratch root (`/tmp` is RAM-backed `tmpfs` on Linux runners).
fn disk_scratch_root() -> PathBuf {
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
const FIXTURE_DOCKERFILE: &str = "\
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

/// The engine-valid [`WorldRootfs`] for `state_dir`, seeded from the shared fixture cache.
///
/// [`BtrfsEngine`] only accepts a rootfs whose `app_rootfs` is the content-addressed
/// `<state_dir>/cache/rootfs/<digest>/app` tree it owns (`WorldEngine::clone_world`), while
/// [`fixture_rootfs`] materializes once per test run into one shared cache. The real, immutable
/// `L0` tree is therefore hardlinked into each suite's own state directory: byte-identical
/// content, the production path layout, and no second Docker build. Overlayfs only ever reads a
/// lowerdir, so the shared inodes are never written through.
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

/// A short, disk-backed, per-process scratch root for one engine suite.
///
/// Kept short on purpose: `<state>/ctl/<world>/netns.sock` must fit the 107-byte `AF_UNIX` path
/// limit, and parallel suites must never share a directory.
pub fn engine_test_dir(label: &str) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in label.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let dir = disk_scratch_root().join("engine").join(format!(
        "{:08x}-{}",
        hash & 0xffff_ffff,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create engine test dir");
    dir
}

/// World environment with the inherited allowlist and a world-scoped `HOME`.
pub fn world_env(home: &Path) -> ExecEnv {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(
        InheritedVar::Path,
        std::env::var("PATH").unwrap_or_else(|_| HELPER_PATH.to_owned()),
    );
    env
}

/// Container environment: the image's `PATH` plus the minimal locale the service images carry.
///
/// The minimal service images carry no locale archive; `LANG=C.UTF-8` from a developer shell
/// would make Redis `setlocale` fail before it binds the port.
pub fn container_env(path: &str) -> ExecEnv {
    let mut env = ExecEnv::default();
    env.inherit(InheritedVar::Path, path);
    env.inherit(InheritedVar::Lang, "C");
    env
}

/// A retained service log concatenated as lossy text.
pub fn log_text(logs: &ServiceLogs) -> String {
    logs.snapshot()
        .iter()
        .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
        .collect()
}

/// The one postgres state-subdirectory mount under a world's state tree.
pub fn pg_state() -> Vec<StateSubdirMount> {
    vec![StateSubdirMount {
        host_subdir: PathBuf::from("pgdata/main"),
        container_path: PathBuf::from(PG_DATA),
    }]
}

/// One shared engine fixture: a fresh state dir, a real `L0` rootfs, a base snapshot, and the
/// repo/artifact mounts every suite clones worlds with.
pub struct WorldFixture {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub engine: Arc<BtrfsEngine>,
    pub rootfs: WorldRootfs,
    pub snapshot: SnapshotId,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub artifacts: PathBuf,
}

impl WorldFixture {
    pub fn new(label: &str) -> Self {
        Self::with_driver(label, Arc::new(TestTreeDriver::btrfs()))
    }

    pub fn with_driver(label: &str, driver: Arc<TestTreeDriver>) -> Self {
        let root = engine_test_dir(label);
        let state_dir = root.join("state");
        let (uid, gid) = current_uid_gid();
        let engine = Arc::new(
            BtrfsEngine::with_driver(
                &state_dir,
                uid,
                gid,
                env!("CARGO_BIN_EXE_slipfork-world-init"),
                driver as Arc<dyn SubvolumeDriver>,
            )
            .expect("BtrfsEngine over the shared TestTreeDriver"),
        );
        let rootfs = engine_rootfs(&state_dir);
        let src = root.join("state-src");
        std::fs::create_dir_all(src.join("state")).expect("state-src");
        std::fs::write(src.join("state/seed.txt"), b"ancestor\n").expect("seed");
        let snapshot = SnapshotId("base".to_owned());
        engine
            .create_snapshot(&src, &snapshot)
            .expect("base snapshot");
        let home = root.join("home");
        let repo = root.join("repo");
        let artifacts = root.join("artifacts");
        for dir in [&repo, &artifacts] {
            std::fs::create_dir_all(dir).expect("fixture dir");
        }
        Self {
            root,
            state_dir,
            engine,
            rootfs,
            snapshot,
            home,
            repo,
            artifacts,
        }
    }

    pub fn clone_world(
        &self,
        target: WorldTarget,
        ingress: Option<LocalIngressBridge>,
        snapshot: &SnapshotId,
    ) -> WorldHandle {
        self.clone_with(
            target,
            snapshot,
            vec![WorldMount {
                host_path: self.repo.clone(),
                guest_path: self.repo.clone(),
                mode: MountMode::ReadWrite,
            }],
            ingress,
        )
    }

    pub fn world(&self, target: WorldTarget) -> WorldHandle {
        self.clone_world(target, None, &self.snapshot)
    }

    /// A world that also carries the fixture's read-only artifact volume.
    pub fn world_with_artifacts(&self, target: WorldTarget) -> WorldHandle {
        self.clone_with(
            target,
            &self.snapshot,
            vec![
                WorldMount {
                    host_path: self.repo.clone(),
                    guest_path: self.repo.clone(),
                    mode: MountMode::ReadWrite,
                },
                WorldMount {
                    host_path: self.artifacts.clone(),
                    guest_path: self.artifacts.clone(),
                    mode: MountMode::ReadOnly,
                },
            ],
            None,
        )
    }

    fn clone_with(
        &self,
        target: WorldTarget,
        snapshot: &SnapshotId,
        mounts: Vec<WorldMount>,
        ingress: Option<LocalIngressBridge>,
    ) -> WorldHandle {
        self.engine
            .clone_world(&WorldSpec {
                target,
                rootfs: self.rootfs.clone(),
                snapshot: snapshot.clone(),
                mounts,
                base_env: world_env(&self.home),
                local_ingress: ingress,
            })
            .expect("clone world")
    }

    pub fn exec(&self, world: &WorldHandle, script: &str) -> ExecOutput {
        self.exec_in(world, &self.repo, script)
    }

    pub fn exec_in(&self, world: &WorldHandle, cwd: &Path, script: &str) -> ExecOutput {
        let mut chunks: Vec<StreamChunk> = Vec::new();
        self.engine
            .exec_in_world(
                world,
                &["sh".to_owned(), "-c".to_owned(), script.to_owned()],
                cwd,
                &ExecEnv::default(),
                &mut |chunk| chunks.push(chunk),
            )
            .expect("exec inside world")
    }

    pub fn service(&self, world: &WorldHandle, spec: ServiceSpec) -> ServiceHandle {
        self.engine
            .spawn_service(world, &spec)
            .unwrap_or_else(|err| panic!("spawn service '{}': {err}", spec.id))
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
