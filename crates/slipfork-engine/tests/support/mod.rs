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
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver, mount_hint};
use slipfork_engine::rootfs::DockerImageStore;
use slipfork_engine::{BtrfsEngine, InheritedEnv, current_uid_gid};
use slipfork_world::rootfs::{OciRootfs, WorldRootfs};
use slipfork_world::{
    BootLineage, ExecEnv, ExecOutput, InheritedVar, LocalIngressBridge, MountMode, ServiceHandle,
    ServiceLogs, ServiceSpec, SnapshotId, StateSubdirMount, StreamChunk, StreamSource, WorldEngine,
    WorldHandle, WorldMount, WorldSpec,
};
// The one bounded-wait shape and its named deadlines come from the shared testkit, so no suite in
// this crate can drift into a second copy of the poll loop.
pub use slipfork_testkit::pods::ComponentPod;
pub use slipfork_testkit::{POLL_INTERVAL, poll_until_with_interval as poll_until};
use slipfork_world::WorldTarget;

mod rootfs;
pub use rootfs::{engine_rootfs, pinned_crun};

/// Standard `PATH` used when the invoking shell exposes none.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// In-image postgres data directory used by every service suite.
pub const PG_DATA: &str = "/var/lib/postgresql/data";

/// Block for `window` without polling: the suite's named quiet-window primitive.
///
/// An absence assertion ("a stopped writer never grows its state again") needs a window in which
/// nothing is expected to happen, so there is no state to poll for. It is never a readiness proof
/// and never a poll: every readiness proof in these suites goes through [`poll_until`] instead.
pub fn observe_for(window: Duration) {
    std::thread::sleep(window);
}

// Disk-backed scratch root resolved once in the shared testkit (`SLIPFORK_TEST_ROOT`, then
// `$HOME/.cache/slipfork`), so no suite carries a second resolution.
use slipfork_testkit::live::disk_scratch_root;

/// A temporary directory on disk, never on `tmpfs`.
pub fn disk_tempdir(label: &str) -> tempfile::TempDir {
    let root = disk_scratch_root();
    std::fs::create_dir_all(&root).expect("create disk scratch root");
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in(root)
        .expect("disk tempdir")
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

/// The real kernel driver for `state_dir`, or ONE fail-closed line naming the missing substrate.
///
/// `state_dir` is created first, so the `statfs` probe reports the real filesystem instead of a
/// bare `ENOENT`; a non-Btrfs host then panics with the probed path, the found `f_type`, the
/// required [`BTRFS_SUPER_MAGIC`], and the mount remedy. There is no `Option` to return early on,
/// so no protected suite can report green on a host that cannot boot a world.
pub fn require_btrfs(state_dir: &Path) -> KernelBtrfsDriver {
    std::fs::create_dir_all(state_dir)
        .unwrap_or_else(|error| panic!("create the state dir {}: {error}", state_dir.display()));
    let driver = KernelBtrfsDriver::new();
    let facts = driver.probe_facts(state_dir).unwrap_or_else(|error| {
        panic!(
            "slipfork-engine live suites cannot probe {} (statfs failed: {error}); fix: {}",
            state_dir.display(),
            mount_hint(state_dir)
        )
    });
    assert!(
        facts.fstype == BTRFS_SUPER_MAGIC,
        "slipfork-engine live suites require a Btrfs state dir at {} (statfs reports f_type {:#x}; \
         BTRFS_SUPER_MAGIC is {:#x}); fix: {}",
        facts.probed_path.display(),
        facts.fstype,
        BTRFS_SUPER_MAGIC,
        mount_hint(&facts.probed_path)
    );
    driver
}

/// The inherited allowlist this test process would hand to a world. The library never reads
/// `std::env`; every suite resolves it here, exactly as the daemon's composition root does.
pub fn inherited_env() -> InheritedEnv {
    InheritedEnv::new(std::env::vars().filter_map(|(name, value)| {
        InheritedVar::parse(&name).map(|var| (var.name().to_owned(), value))
    }))
}

/// The real engine over [`require_btrfs`]'s driver; every other construction error is a hard fail.
pub fn require_engine(state_dir: &Path, world_init: PathBuf) -> BtrfsEngine {
    let driver = require_btrfs(state_dir);
    let (uid, gid) = current_uid_gid();
    BtrfsEngine::new(state_dir, uid, gid, world_init, driver, inherited_env()).unwrap_or_else(
        |error| {
            panic!(
                "cannot construct the Btrfs engine at {}: {error}",
                state_dir.display()
            )
        },
    )
}

impl WorldFixture {
    /// Build the fixture over the real kernel driver; a non-Btrfs host fails closed through
    /// [`require_btrfs`] with one clear line (never a skipped test, never a fake driver).
    pub fn new(label: &str) -> Self {
        let root = engine_test_dir(label);
        let state_dir = root.join("state");
        let engine = Arc::new(require_engine(
            &state_dir,
            PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init")),
        ));
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
                boot: BootLineage::default(),
            })
            .expect("clone world")
    }

    pub fn exec(&self, world: &WorldHandle, script: &str) -> ExecOutput {
        self.exec_in(world, &self.repo, script)
    }

    pub fn exec_in(&self, world: &WorldHandle, cwd: &Path, script: &str) -> ExecOutput {
        let mut chunks: Vec<StreamChunk> = Vec::new();
        self.exec_argv(
            world,
            &["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd,
            &ExecEnv::default(),
            &mut |chunk| chunks.push(chunk),
        )
    }

    /// Run `argv` inside `world`'s namespaces through the host's `nsenter`, starting in `cwd`.
    pub fn exec_argv(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> ExecOutput {
        nsenter_exec(
            self.engine.as_ref(),
            &self.home,
            world,
            argv,
            cwd,
            overlay_env,
            on_output,
        )
    }

    /// The live world init's host pid, read from the engine's published namespace joins.
    pub fn init_pid(&self, world: &WorldHandle) -> u32 {
        let joins = self
            .engine
            .world_pod_namespaces(world)
            .expect("a live world publishes its namespace joins");
        init_pid_from(&joins)
    }

    /// Plan one component pod exactly the way `slipforkd`'s `PodBoot::component` plans it: the
    /// world base environment overlaid with the pod's own, the engine bundle directory, the
    /// world's published namespace joins, a `VolumePlan` rooted on the world subvolume, and the
    /// exact `cwd` beneath the live worktree.
    pub fn component_spec(&self, world: &WorldHandle, pod: &ComponentPod) -> ServiceSpec {
        pod.spec(
            self.engine.as_ref(),
            world,
            &self.repo,
            &self.rootfs.app_rootfs,
            &world_env(&self.home),
        )
        .unwrap_or_else(|err| panic!("plan component pod '{}': {err}", pod.service()))
    }

    /// Spawn one component pod through the production `ServicePod` path.
    pub fn component(&self, world: &WorldHandle, pod: ComponentPod) -> ServiceHandle {
        let spec = self.component_spec(world, &pod);
        self.engine
            .spawn_service(world, &spec)
            .unwrap_or_else(|err| panic!("spawn service '{}': {err}", spec.id))
    }

    /// The `fx.service(&world, spec)` name every protected journey already uses.
    ///
    /// `spec` is the [`ComponentPod`] [`service_spec`] returns, so the call is exactly
    /// [`Self::component`]: one OCI bundle in the engine's control directory (outside the world
    /// subvolume), the world's published namespace joins, and the world's own `-pgid` group.
    pub fn service(&self, world: &WorldHandle, pod: ComponentPod) -> ServiceHandle {
        self.component(world, pod)
    }
}

/// The live world-init pid named by the engine's published user-namespace join.
pub fn init_pid_from(joins: &slipfork_world::PodNamespaces) -> u32 {
    joins
        .userns
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|pid| pid.to_str())
        .and_then(|pid| pid.parse().ok())
        .expect("the published user namespace join names /proc/<init_pid>/ns/user")
}

/// Run `argv` inside `world`'s namespaces through the host's `nsenter`, starting in `cwd`.
///
/// The world shares the host mount table 1:1, so `cwd` is a real in-world path opened after
/// `setns` (`--wdns`); the child also sees `SLIPFORK_WORLD_STATE` pointing at the world's CoW
/// state directory, the same contract a service container gets.
pub fn nsenter_exec(
    engine: &BtrfsEngine,
    home: &Path,
    world: &WorldHandle,
    argv: &[String],
    cwd: &Path,
    overlay_env: &ExecEnv,
    on_output: &mut dyn FnMut(StreamChunk),
) -> ExecOutput {
    let joins = engine
        .world_pod_namespaces(world)
        .expect("a live world publishes its namespace joins");
    let init_pid = init_pid_from(&joins);
    let state_dir = engine
        .world_subvolume(world)
        .expect("world subvolume")
        .join("state");
    let env = world_env(home).overlaid(overlay_env);
    let mut command = Command::new("nsenter");
    command
        .args([
            format!("--target={init_pid}"),
            "--user".to_owned(),
            "--preserve-credentials".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            format!("--wdns={}", cwd.display()),
            "--".to_owned(),
        ])
        .args(argv)
        .env_clear()
        .env("SLIPFORK_WORLD_STATE", &state_dir)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env.iter() {
        command.env(name, value.as_ref());
    }
    let output = command.output().expect("spawn the world namespace entry");
    if !output.stdout.is_empty() {
        on_output(StreamChunk {
            source: StreamSource::Stdout,
            bytes: output.stdout.clone(),
        });
    }
    if !output.stderr.is_empty() {
        on_output(StreamChunk {
            source: StreamSource::Stderr,
            bytes: output.stderr.clone(),
        });
    }
    ExecOutput {
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The retired `service_spec(id, worktree, readiness, script)` shape, now a [`ComponentPod`].
///
/// The absolute worktree argument stays for call-site parity: a pod's directory is resolved
/// beneath the live world worktree by [`ComponentPod::spec`], and the root (`""`) this shim means
/// is exactly the world's read-write mount of `worktree`. Nothing here degrades to a world
/// process; a suite that needs a custom `argv` builds the pod with [`ComponentPod::new`].
pub fn service_spec(
    name: &str,
    _worktree: &Path,
    readiness_port: Option<u16>,
    script: &str,
) -> ComponentPod {
    let pod = ComponentPod::script(name, script);
    match readiness_port {
        Some(port) => pod.readiness_port(port),
        None => pod,
    }
}

/// Materialize one image reference through the production [`DockerImageStore`].
///
/// `extra_paths` are prepared **inside** the extracted rootfs so `crun` can bind a state or
/// read-only mount at an existing destination (the runtime never mutates a read-only rootfs).
/// The real store owns digest resolution, `docker create`/`export`, `tar -xf`, and the atomic
/// cache install; a missing daemon or failed pull fails the test with the exact fix, never a skip.
pub fn oci_rootfs(image_ref: &str, extra_paths: &[&str]) -> OciRootfs {
    let store = DockerImageStore::new(disk_scratch_root().join("oci-state"));
    let image = store
        .materialize_reference(image_ref, &mut |_| {})
        .unwrap_or_else(|error| {
            panic!("cannot materialize {image_ref} through DockerImageStore:\n{error}")
        });
    for path in extra_paths {
        let target = image.rootfs.join(path.trim_start_matches('/'));
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
    image
}
