//! The real Btrfs world fixture every world-booting suite shares.
//!
//! Nothing here fakes the substrate: [`require_btrfs`] is the production `statfs` magic probe,
//! [`WorldFixture`] materializes a genuine `docker buildx` `L0` rootfs, and every world is a real
//! Btrfs snapshot plus a real `slipfork-world-init` PID 1. A host without Btrfs fails closed with
//! one line naming the missing substrate, so a suite can never report green by skipping.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver, mount_hint};
use slipfork_engine::rootfs::pin::CRUN_PIN_X86_64;
use slipfork_engine::rootfs::{
    BaseImageBuilder, DockerBuildx, DockerImageStore, DockerfileStageBuilder, ElfProbe,
    FsInputHasher, HostCacheRequest, RuntimeAssets, SnowPortContract, WorldRootfsRequest,
};
use slipfork_engine::{BtrfsEngine, InheritedEnv, current_uid_gid};
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, EngineError, ExecEnv, ExecOutput, InheritedVar, MountMode, SnapshotId,
    StreamChunk, WorldEngine, WorldHandle, WorldMount, WorldSpec, WorldTarget,
};

/// Standard `PATH` used when the invoking shell exposes none.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Disk-backed scratch root (`/tmp` is RAM-backed `tmpfs` on Linux runners).
///
/// The one resolution of `SLIPFORK_TEST_ROOT` for every live suite: the VM harness sets it to a
/// short absolute directory (`/var/lib/slipfork/t`) because `<state>/ctl/<world>/netns.sock` must
/// fit the 107-byte `AF_UNIX` limit and a deep `$HOME` overflows it.
pub fn disk_scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        let root = PathBuf::from(root);
        assert!(
            root.is_absolute(),
            "SLIPFORK_TEST_ROOT must be an absolute disk directory, got {}",
            root.display()
        );
        return root;
    }
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-tmp")
}

/// A temporary directory on disk, never on `tmpfs`.
pub fn disk_tempdir(label: &str) -> tempfile::TempDir {
    let root = disk_scratch_root();
    fs::create_dir_all(&root).expect("create disk scratch root");
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in(root)
        .expect("disk tempdir")
}

/// A short, disk-backed, per-process scratch root for one world-booting suite.
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
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create engine test dir");
    dir
}

/// The `slipfork-world-init` binary this build produced.
///
/// Cargo exposes `CARGO_BIN_EXE_*` only inside the owning package's own test targets, so a suite
/// in another crate locates the sibling the workspace build already produced and fails closed with
/// the exact remedy instead of booting a stale or missing helper.
pub fn world_init_binary() -> PathBuf {
    let executable = std::env::current_exe().expect("current test executable");
    let profile_dir = executable
        .parent()
        .and_then(Path::parent)
        .expect("target profile directory");
    let candidate = profile_dir.join("slipfork-world-init");
    assert!(
        candidate.is_file(),
        "{} is missing; build it with `cargo build -p slipfork-engine --bins` (or `make rs-test`) \
         before running a world-booting suite",
        candidate.display()
    );
    candidate
}

/// The real kernel driver for `state_dir`, or ONE fail-closed line naming the missing substrate.
///
/// `state_dir` is created first, so the `statfs` probe reports the real filesystem instead of a
/// bare `ENOENT`; a non-Btrfs host then panics with the probed path, the found `f_type`, the
/// required [`BTRFS_SUPER_MAGIC`], and the mount remedy. There is no `Option` to return early on,
/// so no protected suite can report green on a host that cannot boot a world.
pub fn require_btrfs(state_dir: &Path) -> KernelBtrfsDriver {
    fs::create_dir_all(state_dir)
        .unwrap_or_else(|error| panic!("create the state dir {}: {error}", state_dir.display()));
    let driver = KernelBtrfsDriver::new();
    let facts = driver.probe_facts(state_dir).unwrap_or_else(|error| {
        panic!(
            "slipfork live suites cannot probe {} (statfs failed: {error}); fix: {}",
            state_dir.display(),
            mount_hint(state_dir)
        )
    });
    assert!(
        facts.fstype == BTRFS_SUPER_MAGIC,
        "slipfork live suites require a Btrfs state dir at {} (statfs reports f_type {:#x}; \
         BTRFS_SUPER_MAGIC is {:#x}); fix: {}",
        facts.probed_path.display(),
        facts.fstype,
        BTRFS_SUPER_MAGIC,
        mount_hint(&facts.probed_path)
    );
    driver
}

/// Path of the pinned static `crun`, downloading and verifying it on a cold cache.
pub fn pinned_crun() -> PathBuf {
    let cache = PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime");
    fs::create_dir_all(&cache).expect("create runtime cache");
    let path = cache.join(CRUN_PIN_X86_64.file_name);
    if verify_sha256(&path) {
        return path;
    }
    let temporary = cache.join(format!("{}.download", CRUN_PIN_X86_64.file_name));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(CRUN_PIN_X86_64.release_url)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to fetch the pinned static crun: {error}\n  fix: install curl, or place \
                 {} (sha256 {}) in {}",
                CRUN_PIN_X86_64.file_name,
                CRUN_PIN_X86_64.sha256,
                cache.display()
            )
        });
    assert!(
        status.success(),
        "failed to download the pinned static crun from {}\n  fix: place {} (sha256 {}) in {}",
        CRUN_PIN_X86_64.release_url,
        CRUN_PIN_X86_64.file_name,
        CRUN_PIN_X86_64.sha256,
        cache.display()
    );
    assert!(
        verify_sha256(&temporary),
        "downloaded crun does not match the compile-time pin {}",
        CRUN_PIN_X86_64.sha256
    );
    fs::rename(&temporary, &path).expect("install pinned crun");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make crun executable");
    path
}

fn verify_sha256(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes)) == CRUN_PIN_X86_64.sha256
}

/// A real [`BtrfsEngine`] over `state_dir`, without materializing an `L0` rootfs.
///
/// The fd-3 activation probe only needs the engine's registry identity and a private socket, so it
/// must not pay for (or depend on) a Docker build. Construction still probes Btrfs first, so a
/// non-Btrfs host fails closed through the one-line [`require_btrfs`] refusal.
pub fn engine_at(state_dir: &Path) -> Arc<BtrfsEngine> {
    fs::create_dir_all(state_dir)
        .unwrap_or_else(|error| panic!("create the state dir {}: {error}", state_dir.display()));
    let driver = require_btrfs(state_dir);
    let (uid, gid) = current_uid_gid();
    Arc::new(
        BtrfsEngine::new(
            state_dir.to_path_buf(),
            uid,
            gid,
            world_init_binary(),
            driver,
            inherited_env(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "cannot construct the Btrfs engine at {}: {error}",
                state_dir.display()
            )
        }),
    )
}

/// Test fixture `Dockerfile`: a real supported `L0` stage plus the client tools a suite scripts.
const FIXTURE_DOCKERFILE: &str = "\
FROM alpine:3.21 AS app
RUN apk add --no-cache bash coreutils python3
WORKDIR /workspace
";

/// Marker file proving a previously materialized fixture is complete.
const FIXTURE_MARKER: &str = ".slipfork/bin/slipfork-world-init";

/// A real [`WorldRootfs`] materialized through the production [`DockerfileStageBuilder`].
fn fixture_rootfs() -> WorldRootfs {
    static ROOTFS: OnceLock<WorldRootfs> = OnceLock::new();
    ROOTFS
        .get_or_init(|| {
            let dir = disk_scratch_root().join("engine-fixture");
            let context = dir.join("context");
            fs::create_dir_all(&context).expect("create fixture context");
            fs::write(context.join("Dockerfile"), FIXTURE_DOCKERFILE)
                .expect("write fixture Dockerfile");
            let world_init = world_init_binary();
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
                    panic!("the live fixture rootfs could not be materialized:\n{error}")
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
/// `<state_dir>/cache/rootfs/<digest>/app` tree it owns, while the fixture materializes once per
/// test run into one shared cache. The immutable `L0` tree is therefore hardlinked into the
/// suite's own state directory: byte-identical content, the production path layout, and no second
/// Docker build. Overlayfs only ever reads a lowerdir, so the shared inodes are never written.
fn engine_rootfs(state_dir: &Path) -> WorldRootfs {
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

/// Hardlink every file of `source` into `destination` (a read-only overlayfs lowerdir).
fn link_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, target)?;
        } else if file_type.is_file() && fs::hard_link(entry.path(), &target).is_err() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The inherited allowlist this test process would hand to a world.
pub fn inherited_env() -> InheritedEnv {
    InheritedEnv::new(std::env::vars().filter_map(|(name, value)| {
        InheritedVar::parse(&name).map(|var| (var.name().to_owned(), value))
    }))
}

/// World environment with the inherited allowlist and a world-scoped `HOME`.
fn world_env(home: &Path) -> ExecEnv {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(
        InheritedVar::Path,
        std::env::var("PATH").unwrap_or_else(|_| HELPER_PATH.to_owned()),
    );
    env
}

/// One real engine, one real `L0` rootfs, and one base snapshot under a fresh Btrfs state root.
///
/// Construction probes Btrfs **before** materializing anything, so the one-line
/// [`require_btrfs`] refusal is the first failure on a non-Btrfs host.
pub struct WorldFixture {
    root: PathBuf,
    state_dir: PathBuf,
    engine: Arc<BtrfsEngine>,
    rootfs: WorldRootfs,
    snapshot: SnapshotId,
    home: PathBuf,
    repo: PathBuf,
    /// Every world this fixture booted, destroyed on drop so an early assertion never leaks one.
    worlds: Mutex<Vec<WorldHandle>>,
}

impl WorldFixture {
    /// Build the fixture over the real kernel driver and the real `docker buildx` `L0` rootfs.
    pub fn new(label: &str) -> Self {
        let root = engine_test_dir(label);
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).expect("state root");
        let engine = engine_at(&state_dir);
        let rootfs = engine_rootfs(&state_dir);
        let source = root.join("state-src");
        fs::create_dir_all(source.join("state")).expect("state-src");
        fs::write(source.join("state/seed.txt"), b"ancestor\n").expect("seed");
        let snapshot = SnapshotId("base".to_owned());
        engine
            .create_snapshot(&source, &snapshot)
            .expect("base snapshot");
        let home = root.join("home");
        let repo = root.join("repo");
        for dir in [&home, &repo] {
            fs::create_dir_all(dir).expect("fixture dir");
        }
        Self {
            root,
            state_dir,
            engine,
            rootfs,
            snapshot,
            home,
            repo,
            worlds: Mutex::new(Vec::new()),
        }
    }

    /// The real engine the suite injects into its registry.
    pub fn engine(&self) -> &Arc<BtrfsEngine> {
        &self.engine
    }

    /// The Btrfs state root (the directory `HttpIngress::bind` writes its socket into).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The host repository directory mounted read-write into every world.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Boot one real world from the fixture's base snapshot, tracking it for reaping on drop.
    pub fn clone_world(&self, target: WorldTarget) -> WorldHandle {
        let world = self
            .engine
            .clone_world(&WorldSpec {
                target,
                rootfs: self.rootfs.clone(),
                snapshot: self.snapshot.clone(),
                mounts: vec![WorldMount {
                    host_path: self.repo.clone(),
                    guest_path: self.repo.clone(),
                    mode: MountMode::ReadWrite,
                }],
                base_env: world_env(&self.home),
                local_ingress: None,
                boot: BootLineage::default(),
            })
            .expect("clone a real world");
        self.worlds
            .lock()
            .expect("tracked worlds")
            .push(world.clone());
        world
    }

    /// Run `sh -c <script>` inside a real service pod joined to `world`'s published namespaces.
    pub fn exec(&self, world: &WorldHandle, script: &str) -> Result<ExecOutput, EngineError> {
        let pod = crate::pods::ServicePod::spawn(
            self.engine.as_ref(),
            world,
            &self.repo,
            &self.rootfs.app_rootfs,
            crate::pods::PodPlan::new("fixture-exec", &["sh", "-c", "exec sleep 600"]),
        )?;
        let mut chunks: Vec<StreamChunk> = Vec::new();
        let output = self.engine.exec_in_service(
            pod.handle(),
            &["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            &ExecEnv::default(),
            &mut |chunk| chunks.push(chunk),
        );
        let _ = self.engine.stop_service(pod.handle());
        output
    }

    /// Boot one real service pod in `world` through the same engine path `slipforkd` uses.
    ///
    /// [`PodPlan`](crate::pods::PodPlan)'s directory is resolved beneath the fixture's read-write
    /// worktree ([`WorldFixture::repo`]), so the pod's live bind (and any declared CoW volume)
    /// behaves exactly as it does in a daemon-booted world. The returned handle carries the bundle
    /// directory, the world init's published namespace joins, and the pod's own `-pgid` readers.
    pub fn spawn_pod(
        &self,
        world: &WorldHandle,
        plan: crate::pods::PodPlan,
    ) -> crate::pods::ServicePod<'_> {
        let id = plan.service().to_owned();
        crate::pods::ServicePod::spawn(
            self.engine.as_ref(),
            world,
            &self.repo,
            &self.rootfs.app_rootfs,
            plan,
        )
        .unwrap_or_else(|error| panic!("cannot spawn service pod '{id}': {error}"))
    }
}

impl Drop for WorldFixture {
    /// Destroy every world this fixture booted, so a failed assertion never leaks a world or its
    /// `world-init` process group past the test.
    fn drop(&mut self) {
        let worlds = std::mem::take(&mut *self.worlds.lock().expect("tracked worlds"));
        for world in worlds {
            // A world the registry already released is destroyed exactly once; the second call
            // fails closed on the missing handle and must not mask the test outcome.
            let _ = self.engine.destroy_world(world);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
