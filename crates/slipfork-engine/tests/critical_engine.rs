//! Protected critical tests for `BtrfsEngine` (`R10`, `R11`, `H-011`).
//!
//! Every suite drives `BtrfsEngine::with_driver` over the shared
//! [`slipfork_testkit::btrfs::TestTreeDriver`], which emulates the [`SubvolumeDriver`] contract in
//! real directories (`ARCHITECTURE.md` §1), so the CoW substrate runs unchanged on any host. What
//! stays real is everything above that seam: the unprivileged
//! `userns`/`netns`/`ipcns`/`pidns`/`mntns` world PID 1 (`slipfork-world-init`), `nsenter` entry,
//! `pivot_root` mount plumbing, service supervision, and the `AF_UNIX` / `SCM_RIGHTS` descriptor
//! handoff — never a mocked engine and never a fixed sleep as a readiness proof.
//!
//! The real `BTRFS_IOC_*` kernel path (read-only snapshots, writable clones, `--ipc` isolation
//! across parallel containers) is `critical_btrfs_live.rs`, which runs on the Btrfs host.

pub mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use slipfork_engine::btrfs::SubvolumeDriver;
use slipfork_engine::conformance::assert_world_engine_conformance;
use slipfork_engine::{
    BtrfsEngine, EngineError, ExecEnv, InheritedVar, LocalIngressBridge, MountMode, ServiceRuntime,
    ServiceSpec, ServiceState, SnapshotId, StreamChunk, WorldEngine, WorldEntryCommand,
    WorldHandle, WorldMount, WorldSpec, current_uid_gid,
};
use slipfork_testkit::btrfs::{SubvolumeOp, TestTreeDriver};
use slipfork_testkit::poll_until;
use slipfork_world::WorldTarget;
use support::{WorldFixture, engine_test_dir, log_text, world_env};

const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const XFS_SUPER_MAGIC: i64 = 0x5846_5342;

/// Observation window for "a stopped writer never grows its state again" (an absence assertion
/// needs a window; it is never used as a readiness proof).
const QUIET_WINDOW: Duration = Duration::from_millis(150);

fn snap(label: &str) -> SnapshotId {
    SnapshotId(label.to_owned())
}

fn target_pid(entry: &WorldEntryCommand) -> u32 {
    // `entry_command` names the world's persistent PID 1 in `nsenter --target=<init_pid>`.
    entry
        .args_prefix
        .iter()
        .find_map(|arg| arg.strip_prefix("--target="))
        .and_then(|pid| pid.parse().ok())
        .expect("entry command must carry nsenter --target=<init_pid>")
}

fn service_spec(id: &str, cwd: &Path, port: Option<u16>, script: &str) -> ServiceSpec {
    ServiceSpec {
        id: id.to_owned(),
        runtime: ServiceRuntime::WorldProcess {
            argv: vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: cwd.to_path_buf(),
        },
        env: ExecEnv::default(),
        readiness_port: port,
    }
}

#[test]
fn r10_a_non_btrfs_state_dir_fails_closed_without_a_silent_fallback() {
    let root = engine_test_dir("non-btrfs");
    let state_dir = root.join("state");
    let (uid, gid) = current_uid_gid();
    let err = BtrfsEngine::with_driver(
        &state_dir,
        uid,
        gid,
        env!("CARGO_BIN_EXE_slipfork-world-init"),
        Arc::new(TestTreeDriver::non_btrfs(XFS_SUPER_MAGIC)) as Arc<dyn SubvolumeDriver>,
    )
    .expect_err("a non-Btrfs state dir must fail the boot gate closed");
    match err {
        EngineError::BtrfsRequired {
            path,
            actual_fstype,
            mount_hint,
        } => {
            assert_eq!(path, state_dir);
            assert_eq!(
                actual_fstype, XFS_SUPER_MAGIC,
                "the refusal must report the probed f_type"
            );
            assert_eq!(mount_hint, slipfork_engine::btrfs::BTRFS_MOUNT_HINT);
            assert!(
                mount_hint.contains("user_subvol_rm_allowed,noatime"),
                "the refusal names the exact mount remediation: {mount_hint}"
            );
        }
        other => panic!("expected BtrfsRequired, got {other:?}"),
    }
    assert!(
        !state_dir.exists(),
        "a refused engine creates no state directory at all"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn r10_r11_btrfs_conformance_cow_isolation_uid_and_parallel_same_port() {
    let fx = WorldFixture::new("cow-isolation");
    let (uid, gid) = current_uid_gid();

    // 1. Shared WorldEngine contract (fail-closed errors, redaction, opaque handles, seal-once).
    assert_world_engine_conformance(fx.engine.as_ref(), &fx.root, &fx.rootfs);

    // 2. Two simultaneous clones of the same snapshot.
    let workspace_a = fx.repo.clone();
    let workspace_b = fx.root.join("ws-b");
    let home_b = fx.root.join("home-b");
    fs::create_dir_all(&workspace_b).expect("ws-b");
    let world_a = fx.clone_world(WorldTarget::Canonical, None, &fx.snapshot);
    let world_b = fx
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::UserFork("b".to_owned()),
            rootfs: fx.rootfs.clone(),
            snapshot: fx.snapshot.clone(),
            mounts: vec![WorldMount {
                host_path: workspace_b.clone(),
                guest_path: workspace_b.clone(),
                mode: MountMode::ReadWrite,
            }],
            base_env: world_env(&home_b),
            local_ingress: None,
        })
        .expect("clone world b");

    // 3. World A's writes (state + shared workspace) never leak into world B; files stay host-owned.
    let mut streamed: Vec<StreamChunk> = Vec::new();
    let out = fx
        .engine
        .exec_in_world(
            &world_a,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "echo mutated-a >> \"$SLIPFORK_WORLD_STATE/seed.txt\"; \
                 printf 'host-owned\\n' > generated.txt; echo \"$HOME\"; id -u; id -g"
                    .to_owned(),
            ],
            &workspace_a,
            &ExecEnv::default(),
            &mut |chunk| streamed.push(chunk),
        )
        .expect("exec world a");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(!streamed.is_empty(), "exec must stream output chunks");
    let mut lines = out.stdout.lines();
    assert_eq!(lines.next(), Some(fx.home.display().to_string().as_str()));
    assert_eq!(lines.next(), Some(uid.to_string().as_str()));
    assert_eq!(lines.next(), Some(gid.to_string().as_str()));
    let generated = fs::metadata(workspace_a.join("generated.txt")).expect("generated.txt");
    assert_eq!(generated.uid(), uid, "files must stay host-owned");
    assert_eq!(generated.gid(), gid, "files must stay host-owned");
    let out = fx.exec_in(
        &world_b,
        &workspace_b,
        "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(
        out.stdout, "ancestor\n",
        "world B must never see world A's writes"
    );

    // 4. Both worlds bind 127.0.0.1:18432 at the same time in parallel threads (zero EADDRINUSE).
    let bind_py = "import socket, time\n\
                   s = socket.socket()\n\
                   s.bind((\"127.0.0.1\", 18432))\n\
                   s.listen(1)\n\
                   time.sleep(0.15)\n\
                   print(\"BOUND_OK\", s.getsockname()[1])\n";
    let bind_in_world = |world: WorldHandle, cwd: PathBuf| {
        let engine = Arc::clone(&fx.engine);
        thread::spawn(move || {
            engine
                .exec_in_world(
                    &world,
                    &["python3".to_owned(), "-c".to_owned(), bind_py.to_owned()],
                    &cwd,
                    &ExecEnv::default(),
                    &mut |_| {},
                )
                .expect("parallel exec")
        })
    };
    let out_a = bind_in_world(world_a.clone(), workspace_a.clone())
        .join()
        .expect("join a");
    let out_b = bind_in_world(world_b.clone(), workspace_b.clone())
        .join()
        .expect("join b");
    assert_eq!(out_a.exit_code, 0, "stderr: {}", out_a.stderr);
    assert_eq!(out_b.exit_code, 0, "stderr: {}", out_b.stderr);
    assert!(out_a.stdout.contains("BOUND_OK 18432"));
    assert!(out_b.stdout.contains("BOUND_OK 18432"));

    fx.engine.destroy_world(world_a).expect("destroy world a");
    fx.engine.destroy_world(world_b).expect("destroy world b");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn one_writable_subvolume_per_world_with_ro_snapshot_lineage_and_host_ownership() {
    let driver = Arc::new(TestTreeDriver::btrfs());
    let fx = WorldFixture::with_driver("layout", Arc::clone(&driver));
    let (uid, gid) = current_uid_gid();
    let snapshots = fx.state_dir.join("snapshots");
    let worlds = fx.state_dir.join("worlds");

    // 1. A snapshot is one sealed subvolume; the plain import directory left no staging tree.
    assert_eq!(
        driver.list_children(&snapshots).expect("snapshots"),
        vec![snapshots.join("base")],
        "one sealed snapshot subvolume, no staging residue"
    );
    assert_eq!(
        fs::metadata(snapshots.join("base"))
            .expect("snapshot metadata")
            .permissions()
            .mode()
            & 0o200,
        0,
        "a read-only snapshot carries no owner-write bit"
    );
    assert!(
        fs::write(snapshots.join("base/new-file"), b"nope").is_err(),
        "a new entry at the sealed snapshot root fails"
    );

    let world_a = fx.world(WorldTarget::Canonical);
    let after_a = driver.list_children(&worlds).expect("worlds after a");
    assert_eq!(after_a, vec![worlds.join("canonical-1")]);
    let world_b = fx.world(WorldTarget::UserFork("sib".to_owned()));
    let after_b = driver.list_children(&worlds).expect("worlds after b");
    assert_eq!(after_b.len(), 2, "each world is exactly one subvolume");

    // 2. One `snapshot_writable` per world, always from the same read-only snapshot.
    let writable: Vec<(PathBuf, PathBuf)> = driver
        .journal()
        .iter()
        .filter_map(|op| match op {
            SubvolumeOp::SnapshotWritable { source, dest } => Some((source.clone(), dest.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(writable.len(), 2, "one writable clone per world");
    for (source, dest) in &writable {
        assert_eq!(*source, snapshots.join("base"));
        assert_eq!(
            dest.parent(),
            Some(worlds.as_path()),
            "a world subvolume only ever lives under worlds/"
        );
    }

    // 3. The unified layout inside the one subvolume, plus disjoint control state.
    for world_dir in &after_b {
        let rootfs = world_dir.join("rootfs");
        assert!(
            fs::symlink_metadata(&rootfs)
                .expect("rootfs reference")
                .file_type()
                .is_symlink(),
            "the rootfs is a reference, never a second copy of the image"
        );
        assert_eq!(
            fs::read_link(&rootfs).expect("rootfs link target"),
            fx.rootfs.app_rootfs
        );
        for category in ["pgdata", "redis", "s3"] {
            let dir = world_dir.join("state").join(category);
            assert!(
                dir.is_dir(),
                "state lineage directory missing: {}",
                dir.display()
            );
        }
        assert!(
            driver
                .list_children(world_dir)
                .expect("nested subvolumes")
                .is_empty(),
            "one world is one subvolume: no nested subvolumes"
        );
        assert!(
            !world_dir.join("netns.sock").exists(),
            "the dial socket lives in ctl/, so a seal never captures it"
        );
        for path in [world_dir.clone(), world_dir.join("state")] {
            let metadata = fs::metadata(&path).expect("world tree metadata");
            assert_eq!(
                (metadata.uid(), metadata.gid()),
                (uid, gid),
                "world trees are $UID:$GID-owned: {}",
                path.display()
            );
        }
    }
    let ctl_entries = fs::read_dir(fx.state_dir.join("ctl")).expect("ctl dir");
    let mut sockets = Vec::new();
    for entry in ctl_entries {
        let socket = entry.expect("ctl entry").path().join("netns.sock");
        assert!(
            socket.exists(),
            "every world owns a ctl socket: {}",
            socket.display()
        );
        sockets.push(socket);
    }
    assert_eq!(sockets.len(), 2);

    // 4. Sibling clones of one read-only snapshot share nothing writable (state-subdir lineage).
    let world_a_dir = after_b
        .iter()
        .find(|path| path.ends_with("canonical-1"))
        .expect("world a subvolume")
        .clone();
    let world_b_dir = after_b
        .iter()
        .find(|path| path.ends_with("fork-sib-2"))
        .expect("world b subvolume")
        .clone();
    let leaf = world_a_dir.join("state/pgdata/main");
    fs::create_dir_all(&leaf).expect("state leaf");
    fs::write(leaf.join("tuple"), b"a-row\n").expect("state leaf row");
    assert!(
        !world_b_dir.join("state/pgdata/main/tuple").exists(),
        "a sibling clone never sees another clone's state leaf"
    );
    assert!(
        !snapshots.join("base/state/pgdata/main/tuple").exists(),
        "the sealed snapshot never sees a clone's writes"
    );

    // 5. Seal-once freeze: the frozen world becomes the parent of a new lineage.
    fx.engine
        .freeze_world(&world_a, &snap("sealed"))
        .expect("seal world a");
    assert_eq!(
        driver
            .list_children(&snapshots)
            .expect("snapshots after seal"),
        vec![snapshots.join("base"), snapshots.join("sealed")]
    );
    let world_c = fx.clone_world(
        WorldTarget::UserFork("sealed".to_owned()),
        None,
        &snap("sealed"),
    );
    let out = fx.exec(&world_c, "cat \"$SLIPFORK_WORLD_STATE/pgdata/main/tuple\"");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout, "a-row\n",
        "a clone of the sealed world inherits its state lineage"
    );
    let err = fx
        .engine
        .freeze_world(&world_a, &snap("sealed-again"))
        .expect_err("seal-once: an already-frozen world must fail closed");
    assert!(matches!(err, EngineError::WorldNotFreezable { .. }));

    // 6. Teardown: writable worlds go, then a sealed snapshot is cleared read-only before destroy.
    fx.engine.destroy_world(world_c).expect("destroy world c");
    fx.engine.destroy_world(world_b).expect("destroy world b");
    fx.engine.destroy_world(world_a).expect("destroy world a");
    assert!(
        driver
            .list_children(&worlds)
            .expect("worlds after destroy")
            .is_empty(),
        "no world subvolume survives its destroy_world"
    );
    driver
        .destroy_subvolume(&snapshots, "sealed")
        .expect("destroy a sealed snapshot");
    let journal = driver.journal();
    let cleared = journal
        .iter()
        .position(|op| matches!(op, SubvolumeOp::ClearReadonly { path } if path == &snapshots.join("sealed")))
        .expect("the read-only flag is cleared before destroy");
    let destroyed = journal
        .iter()
        .position(|op| matches!(op, SubvolumeOp::DestroySubvolume { path } if path == &snapshots.join("sealed")))
        .expect("the sealed snapshot is destroyed");
    assert!(
        cleared < destroyed,
        "clear read-only must precede destroy: {journal:?}"
    );
    assert!(!snapshots.join("sealed").exists());
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn a_rootfs_outside_the_content_addressed_cache_is_refused_before_a_subvolume_exists() {
    let driver = Arc::new(TestTreeDriver::btrfs());
    let fx = WorldFixture::with_driver("rootfs-cache", Arc::clone(&driver));
    let mut rootfs = fx.rootfs.clone();
    rootfs.app_rootfs = fx.root.join("somewhere-else/app");
    let err = fx
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs,
            snapshot: fx.snapshot.clone(),
            mounts: Vec::new(),
            base_env: world_env(&fx.home),
            local_ingress: None,
        })
        .expect_err("a rootfs outside the content-addressed cache must fail closed");
    assert!(
        matches!(err, EngineError::InvalidWorldSpec { .. }),
        "expected InvalidWorldSpec, got {err:?}"
    );
    assert!(
        driver
            .list_children(&fx.state_dir.join("worlds"))
            .expect("worlds")
            .is_empty(),
        "a refused spec creates no world subvolume"
    );
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn multi_mount_repo_rw_and_readonly_artifact_volume_are_real_bind_mounts() {
    let fx = WorldFixture::new("mounts");
    let (uid, gid) = current_uid_gid();
    let artifacts = fx.root.join("artifacts");
    fs::create_dir_all(&artifacts).expect("artifacts");
    fs::write(artifacts.join("rows.txt"), b"artifact-row\n").expect("artifact row");
    let home = fx.root.join("home-mounts");

    let world = fx
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::UserFork("mounts".to_owned()),
            rootfs: fx.rootfs.clone(),
            snapshot: fx.snapshot.clone(),
            mounts: vec![
                WorldMount {
                    host_path: fx.repo.clone(),
                    guest_path: fx.repo.clone(),
                    mode: MountMode::ReadWrite,
                },
                WorldMount {
                    host_path: artifacts.clone(),
                    guest_path: artifacts.clone(),
                    mode: MountMode::ReadOnly,
                },
            ],
            base_env: world_env(&home),
            local_ingress: None,
        })
        .expect("clone multi-mount world");

    let out = fx.exec(&world, &format!("cat {}/rows.txt", artifacts.display()));
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("artifact-row"),
        "stdout: {}",
        out.stdout
    );

    let out = fx.exec(
        &world,
        &format!(
            "cat {}/rows.txt && echo world-write > from-world.txt",
            artifacts.display()
        ),
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("artifact-row"));
    let written = fs::metadata(fx.repo.join("from-world.txt")).expect("RW mount write");
    assert_eq!(written.uid(), uid);
    assert_eq!(written.gid(), gid);

    // The read-only artifact volume really is read-only inside the world.
    let out = fx.exec(
        &world,
        &format!("echo nope > {}/rows.txt", artifacts.display()),
    );
    assert_ne!(out.exit_code, 0, "RO writes must fail closed");
    assert!(
        fs::read_to_string(artifacts.join("rows.txt"))
            .expect("artifact still intact")
            .contains("artifact-row")
    );

    // Nested read-write mounts are refused before any world state is materialized.
    let nested = WorldSpec {
        target: WorldTarget::UserFork("nested".to_owned()),
        rootfs: fx.rootfs.clone(),
        snapshot: fx.snapshot.clone(),
        mounts: vec![
            WorldMount {
                host_path: fx.repo.clone(),
                guest_path: fx.repo.clone(),
                mode: MountMode::ReadWrite,
            },
            WorldMount {
                host_path: fx.repo.join("artifacts"),
                guest_path: fx.repo.join("artifacts"),
                mode: MountMode::ReadWrite,
            },
        ],
        base_env: world_env(&home),
        local_ingress: None,
    };
    let err = fx
        .engine
        .clone_world(&nested)
        .expect_err("nested RW mounts must fail closed");
    assert!(matches!(err, EngineError::OverlappingMounts { .. }));

    fx.engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn freeze_lineage_seals_once_and_sibling_clones_stay_isolated() {
    let fx = WorldFixture::new("freeze");
    let origin = fx.world(WorldTarget::Canonical);
    let out = fx.exec(
        &origin,
        "echo migrated >> \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);

    fx.engine
        .freeze_world(&origin, &snap("seed-frozen"))
        .expect("seal origin world");
    let err = fx
        .engine
        .freeze_world(&origin, &snap("seed-frozen-again"))
        .expect_err("an already-frozen world must fail closed");
    assert!(matches!(err, EngineError::WorldNotFreezable { .. }));

    let frozen = snap("seed-frozen");
    let sibling_a = fx.clone_world(WorldTarget::UserFork("sib".to_owned()), None, &frozen);
    let sibling_b = fx.clone_world(WorldTarget::UserFork("sib2".to_owned()), None, &frozen);

    let out = fx.exec(
        &sibling_a,
        "echo sibling-mutation >> \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    let out = fx.exec(&sibling_b, "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout, "ancestor\nmigrated\n",
        "a sibling clone of the frozen snapshot must never see another clone's writes"
    );

    // Freeze-after-destroy and double-destroy both fail closed.
    let destroyed = sibling_a.clone();
    fx.engine
        .destroy_world(sibling_a)
        .expect("destroy sibling a");
    let err = fx
        .engine
        .freeze_world(&destroyed, &snap("never"))
        .expect_err("a destroyed world cannot be frozen");
    assert!(matches!(err, EngineError::WorldGone { .. }));
    let err = fx
        .engine
        .destroy_world(destroyed)
        .expect_err("freeze-then-destroy double-free must fail closed");
    assert!(matches!(err, EngineError::WorldGone { .. }));

    fx.engine
        .destroy_world(sibling_b)
        .expect("destroy sibling b");
    fx.engine.destroy_world(origin).expect("destroy origin");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn redacted_exec_env_and_opaque_world_handles() {
    const SECRET: &str = "super-secret-token-value";
    const OVERLAY_SECRET: &str = "overlay-secret-value";
    let mut env = ExecEnv::world_scoped("/w/home");
    env.inherit(InheritedVar::Path, "/usr/bin");
    env.set_secret("API_TOKEN", SECRET).expect("secret");
    env.set("APP_URL", "http://acme.localhost").expect("plain");

    let rendered = format!("{env:?}");
    assert!(
        !rendered.contains(SECRET),
        "Debug leaked a secret: {rendered}"
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(
        rendered.contains("http://acme.localhost"),
        "plain vars stay readable"
    );

    // Overlay wins, including a plain/secret rebinding of the same name, and never renders either
    // binding's secret value.
    let mut overlay = ExecEnv::default();
    overlay
        .set_secret("API_TOKEN", OVERLAY_SECRET)
        .expect("secret");
    overlay
        .set("APP_URL", "http://overlay.localhost")
        .expect("plain");
    let merged = env.overlaid(&overlay);
    assert!(!format!("{merged:?}").contains(SECRET));
    assert!(!format!("{merged:?}").contains(OVERLAY_SECRET));

    // The 0600 IPC wire form is the only carrier that may contain the plaintext secret.
    let wire = serde_json::to_string(&env).expect("serialize ExecEnv");
    assert!(wire.contains(SECRET));
    let round_trip: ExecEnv = serde_json::from_str(&wire).expect("deserialize ExecEnv");
    assert_eq!(round_trip, env);
    let outside_allowlist =
        r#"{"home":null,"inherited":{"CARGO_HOME":"/x"},"plain":{},"secrets":{}}"#;
    assert!(serde_json::from_str::<ExecEnv>(outside_allowlist).is_err());
    let both_bindings = r#"{"home":null,"inherited":{},"plain":{"A":"1"},"secrets":{"A":"2"}}"#;
    assert!(serde_json::from_str::<ExecEnv>(both_bindings).is_err());

    // A WorldSpec carrying secrets never renders them either.
    let fx = WorldFixture::new("redact");
    let full = WorldSpec {
        target: WorldTarget::Canonical,
        rootfs: fx.rootfs.clone(),
        snapshot: snap("whatever"),
        mounts: vec![],
        base_env: env.clone(),
        local_ingress: None,
    };
    assert!(!format!("{full:?}").contains(SECRET));

    // Handles are opaque (stable Debug across clones, no filesystem/env payload) and the real
    // child-process boundary is the only place the effective `base ⊕ overlay` values materialize.
    let mut base_env = world_env(&fx.home);
    base_env.set_secret("API_TOKEN", SECRET).expect("secret");
    base_env
        .set("APP_URL", "http://acme.localhost")
        .expect("plain");
    let world = fx
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: fx.rootfs.clone(),
            snapshot: fx.snapshot.clone(),
            mounts: vec![],
            base_env,
            local_ingress: None,
        })
        .expect("clone");
    let mut chunks = Vec::new();
    let out = fx
        .engine
        .exec_in_world(
            &world,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "printf '%s|%s|%s' \"$API_TOKEN\" \"$APP_URL\" \"$HOME\"".to_owned(),
            ],
            &fx.root,
            &overlay,
            &mut |chunk| chunks.push(chunk),
        )
        .expect("exec world");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout,
        format!(
            "{OVERLAY_SECRET}|http://overlay.localhost|{}",
            fx.home.display()
        ),
        "the child sees base ⊕ overlay, with the overlay secret winning"
    );
    assert!(
        !out.stdout.contains(SECRET),
        "the overridden base secret must never reach the child"
    );

    let clone = world.clone();
    assert_eq!(format!("{world:?}"), format!("{clone:?}"));
    assert!(!format!("{world:?}").contains("base"));
    assert!(!format!("{world:?}").contains(fx.root.display().to_string().as_str()));
    assert!(!format!("{world:?}").contains(SECRET));
    fx.engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn persistent_init_pid_stays_stable_across_calls_and_is_reaped_on_destroy() {
    let fx = WorldFixture::new("persistent-init");
    let world = fx.world(WorldTarget::Canonical);

    let entry = fx.engine.entry_command(&world, &fx.repo).expect("entry");
    let init_pid = target_pid(&entry);
    assert_eq!(
        entry.program,
        PathBuf::from("nsenter"),
        "BtrfsEngine enters via a single-threaded nsenter child"
    );
    assert!(
        entry.args_prefix.iter().any(|arg| arg == "--wd=/")
            && entry
                .args_prefix
                .iter()
                .any(|arg| arg == &fx.repo.display().to_string()),
        "entry must apply the requested cwd after entering the world mount namespace: {:?}",
        entry.args_prefix
    );
    assert!(
        Path::new(&format!("/proc/{init_pid}")).exists(),
        "the world PID 1 must be a live host process"
    );

    // The world's `/proc/1` really is our persistent init, not a throw-away exec scope.
    let out = fx.exec(
        &world,
        "tr '\\0' ' ' < /proc/1/cmdline; echo; stat -c %i /proc/1/ns/pid",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("slipfork-world-init"),
        "world PID 1 must be slipfork-world-init: {}",
        out.stdout
    );

    // Consecutive exec/entry calls keep the same PID 1 and the same PID namespace.
    let namespace_before = fx.exec(&world, "stat -c %i /proc/1/ns/pid").stdout;
    for _ in 0..3 {
        let again = fx.engine.entry_command(&world, &fx.repo).expect("entry");
        assert_eq!(target_pid(&again), init_pid, "init_pid must not change");
        let out = fx.exec(&world, "stat -c %i /proc/1/ns/pid");
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(
            out.stdout, namespace_before,
            "every call must land in the same PID namespace"
        );
    }

    fx.engine.destroy_world(world.clone()).expect("destroy");
    let reaped = poll_until(Duration::from_secs(10), || {
        if Path::new(&format!("/proc/{init_pid}")).exists() {
            Err(format!("world init pid {init_pid} is still alive"))
        } else {
            Ok(())
        }
    });
    assert!(
        reaped.is_ok(),
        "destroy_world must reap the world PID 1: {reaped:?}"
    );
    let err = fx
        .engine
        .entry_command(&world, &fx.repo)
        .expect_err("a destroyed world cannot be entered");
    assert!(matches!(err, EngineError::WorldGone { .. }));
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn service_logs_survive_idempotent_stop_and_freeze_seals_only_after_stop() {
    let fx = WorldFixture::new("service");
    let world = fx.world(WorldTarget::Canonical);

    let greeter = fx.service(
        &world,
        service_spec(
            "greeter",
            &fx.repo,
            None,
            "echo service-hello; exec sleep 300",
        ),
    );
    assert_eq!(greeter.state(), ServiceState::Running);
    let logs = fx.engine.service_logs(&world, "greeter").expect("logs");
    let captured = poll_until(Duration::from_secs(10), || {
        if log_text(&logs).contains("service-hello") {
            Ok(())
        } else {
            Err(format!("captured so far: {:?}", log_text(&logs)))
        }
    });
    assert!(
        captured.is_ok(),
        "service stdout must be captured: {captured:?}"
    );

    // `follow` replays retained output and returns once the service stops.
    let follower = {
        let logs = logs.clone();
        thread::spawn(move || {
            let mut chunks: Vec<StreamChunk> = Vec::new();
            logs.follow(&mut |chunk| chunks.push(chunk));
            chunks
        })
    };

    fx.engine.stop_service(&greeter).expect("stop once");
    fx.engine
        .stop_service(&greeter)
        .expect("stop must be idempotent");
    assert_eq!(greeter.state(), ServiceState::Stopped);
    let followed = follower.join().expect("follower thread");
    assert!(
        followed
            .iter()
            .any(|chunk| String::from_utf8_lossy(&chunk.bytes).contains("service-hello")),
        "follow must stream the retained output"
    );
    // Logs survive stop_service (failed worlds stay inspectable).
    assert!(
        log_text(&logs).contains("service-hello"),
        "stop_service must preserve captured logs"
    );
    assert!(logs.is_closed());

    // A live writer is stopped before `state` is sealed, so the snapshot is consistent.
    let writer = fx.service(
        &world,
        service_spec(
            "writer",
            &fx.repo,
            None,
            "while :; do echo tick >> \"$SLIPFORK_WORLD_STATE/live.log\"; sleep 0.01; done",
        ),
    );
    let produced = poll_until(Duration::from_secs(10), || {
        if fx
            .exec(&world, "test -s \"$SLIPFORK_WORLD_STATE/live.log\"")
            .exit_code
            == 0
        {
            Ok(())
        } else {
            Err("writer has not produced state yet".to_owned())
        }
    });
    assert!(
        produced.is_ok(),
        "writer must produce state before the freeze: {produced:?}"
    );
    fx.engine
        .freeze_world(&world, &snap("sealed-after-stop"))
        .expect("freeze with a live service");
    assert_eq!(
        writer.state(),
        ServiceState::Stopped,
        "freeze_world must stop live services before sealing the subvolume"
    );

    let size_at_freeze = fx
        .exec(&world, "wc -c < \"$SLIPFORK_WORLD_STATE/live.log\"")
        .stdout
        .trim()
        .to_owned();
    thread::sleep(QUIET_WINDOW);
    let size_after = fx
        .exec(&world, "wc -c < \"$SLIPFORK_WORLD_STATE/live.log\"")
        .stdout
        .trim()
        .to_owned();
    assert_eq!(
        size_at_freeze, size_after,
        "no service may keep writing state after freeze_world returns"
    );
    assert!(
        size_at_freeze.parse::<u64>().unwrap_or(0) > 0,
        "the frozen snapshot must contain the writer's state"
    );

    // The sealed snapshot is a real, independent clone of that stopped state.
    let sealed = fx.clone_world(
        WorldTarget::UserFork("sealed".to_owned()),
        None,
        &snap("sealed-after-stop"),
    );
    let out = fx.exec(&sealed, "wc -c < \"$SLIPFORK_WORLD_STATE/live.log\"");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        size_at_freeze,
        "the sealed snapshot must equal the state observed after the stop"
    );

    fx.engine.destroy_world(sealed).expect("destroy clone");
    fx.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn readiness_timeout_stops_the_supervisor_and_releases_the_service_id() {
    let fx = WorldFixture::new("ready-timeout");
    let world = fx.world(WorldTarget::Canonical);

    let stalled = service_spec("stall", &fx.repo, Some(18098), "exec sleep 300");
    let err = fx
        .engine
        .spawn_service(&world, &stalled)
        .expect_err("nothing listens on the readiness port");
    assert!(
        format!("{err}").contains("did not accept connections"),
        "expected the typed readiness timeout, got: {err}"
    );

    // The failed spawn aborted (and reaped) its supervisor and closed its retained log.
    let logs = fx
        .engine
        .service_logs(&world, "stall")
        .expect("retained logs survive the aborted spawn");
    assert!(logs.is_closed(), "an aborted spawn must close its log");

    // The id is reusable: a retry that does listen succeeds instead of ServiceAlreadyRunning.
    let server = "import socket\n\
         s = socket.socket()\n\
         s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
         s.bind((\"127.0.0.1\", 18098))\n\
         s.listen(4)\n\
         while True:\n\
         \x20   c, _ = s.accept()\n\
         \x20   c.close()\n";
    let mut retry = service_spec("stall", &fx.repo, Some(18098), "");
    retry.runtime = ServiceRuntime::WorldProcess {
        argv: vec!["python3".to_owned(), "-c".to_owned(), server.to_owned()],
        cwd: fx.repo.clone(),
    };
    let handle = fx.service(&world, retry);
    assert_eq!(handle.state(), ServiceState::Running);

    fx.engine
        .stop_service(&handle)
        .expect("stop retried service");
    fx.engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn dial_world_tcp_reaches_in_world_listener_via_netns_sock_scm_rights() {
    let fx = WorldFixture::new("dial");
    let world = fx.world(WorldTarget::Canonical);

    let echo_server = format!(
        "import socket\n\
         s = socket.socket()\n\
         s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
         s.bind((\"127.0.0.1\", {port}))\n\
         s.listen(8)\n\
         while True:\n\
         \x20   c, _ = s.accept()\n\
         \x20   data = c.recv(4)\n\
         \x20   c.sendall(b\"PONG\" if data == b\"ping\" else b\"WHAT\")\n\
         \x20   c.close()\n",
        port = 18080
    );
    let mut spec = service_spec("echo", &fx.repo, Some(18080), "");
    spec.runtime = ServiceRuntime::WorldProcess {
        argv: vec!["python3".to_owned(), "-c".to_owned(), echo_server],
        cwd: fx.repo.clone(),
    };
    // `spawn_service` only returns once the in-world port accepts connections through
    // `dial_world_tcp`, so readiness itself exercises the `SCM_RIGHTS` handoff.
    let echo = fx.service(&world, spec);
    assert_eq!(echo.state(), ServiceState::Running);

    let mut stream = fx
        .engine
        .dial_world_tcp(&world, 18080)
        .expect("dial in-world listener");
    stream.write_all(b"ping").expect("write request");
    let mut reply = [0_u8; 4];
    stream.read_exact(&mut reply).expect("read reply");
    assert_eq!(&reply, b"PONG", "the handed-off stream must be live");

    // The dial socket is a `0600` pathname AF_UNIX socket in the control directory, outside the
    // world subvolume (a seal could never capture it).
    let ctl_dir = fx.state_dir.join("ctl");
    let sockets: Vec<PathBuf> = fs::read_dir(&ctl_dir)
        .expect("ctl dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("netns.sock")))
        .filter(|path| path.exists())
        .collect();
    assert_eq!(sockets.len(), 1, "exactly one world dial socket");
    assert!(
        !sockets[0].starts_with(fx.state_dir.join("worlds")),
        "the dial socket must live outside the sealed subvolume: {}",
        sockets[0].display()
    );
    let mode = fs::metadata(&sockets[0]).expect("netns.sock").mode() & 0o777;
    assert_eq!(mode, 0o600, "netns.sock must be 0600");

    // A dial for a port nobody listens on fails closed, typed, without inventing a stream.
    let err = fx
        .engine
        .dial_world_tcp(&world, 18099)
        .expect_err("nothing listens on 18099");
    assert!(
        matches!(err, EngineError::DialFailed { port: 18099, .. }),
        "expected DialFailed, got {err:?}"
    );

    fx.engine.destroy_world(world.clone()).expect("destroy");
    let err = fx
        .engine
        .dial_world_tcp(&world, 18080)
        .expect_err("a destroyed world cannot be dialed");
    assert!(matches!(err, EngineError::WorldGone { .. }));
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn two_worlds_isolate_the_same_port_and_dial_independently() {
    let fx = WorldFixture::new("same-port");
    let world_a = fx.world(WorldTarget::UserFork("a".to_owned()));
    let world_b = fx.world(WorldTarget::UserFork("b".to_owned()));

    for (world, payload) in [(&world_a, "world-a"), (&world_b, "world-b")] {
        let server = format!(
            "import socket\n\
             s = socket.socket()\n\
             s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
             s.bind((\"127.0.0.1\", 18432))\n\
             s.listen(8)\n\
             while True:\n\
             \x20   c, _ = s.accept()\n\
             \x20   c.recv(4)\n\
             \x20   c.sendall(b\"{payload}\")\n\
             \x20   c.close()\n"
        );
        let mut spec = service_spec("same-port", &fx.repo, Some(18432), "");
        spec.runtime = ServiceRuntime::WorldProcess {
            argv: vec!["python3".to_owned(), "-c".to_owned(), server],
            cwd: fx.repo.clone(),
        };
        fx.service(world, spec);
    }

    for (world, expected) in [(&world_a, b"world-a"), (&world_b, b"world-b")] {
        let mut stream = fx
            .engine
            .dial_world_tcp(world, 18432)
            .expect("dial same port in world");
        stream.write_all(b"ping").expect("write");
        let mut reply = [0_u8; 7];
        stream.read_exact(&mut reply).expect("read");
        assert_eq!(
            &reply, expected,
            "each world must answer from its own netns"
        );
    }

    fx.engine.destroy_world(world_a).expect("destroy a");
    fx.engine.destroy_world(world_b).expect("destroy b");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn entry_command_enters_the_persistent_init_pid_with_native_stdio() {
    let fx = WorldFixture::new("entry-command");
    let world = fx.world(WorldTarget::Canonical);
    let entry = fx.engine.entry_command(&world, &fx.repo).expect("entry");
    let (uid, _gid) = current_uid_gid();

    // Spawn the engine-supplied prefix directly (the thin CLI does exactly this with inherited
    // stdio): no descriptor handover, no PTY, no daemon involvement.
    let out = Command::new(&entry.program)
        .args(&entry.args_prefix)
        .args([
            "sh",
            "-c",
            "printf 'entered|%s|%s|%s' \"$(id -u)\" \"$(stat -c %i /proc/1/ns/pid)\" \"$(pwd)\"",
        ])
        .env_clear()
        .env("PATH", HELPER_PATH)
        .current_dir(&entry.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn entry command");
    assert!(
        out.status.success(),
        "direct entry failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.starts_with(&format!("entered|{uid}|")),
        "entry must preserve the host uid: {stdout}"
    );
    let via_exec = fx
        .exec(&world, "stat -c %i /proc/1/ns/pid")
        .stdout
        .trim()
        .to_owned();
    assert!(
        stdout.contains(&via_exec),
        "entry and exec must share one PID namespace: {stdout} vs {via_exec}"
    );
    assert!(
        stdout.contains(fx.repo.display().to_string().as_str()),
        "entry must start in the requested cwd: {stdout}"
    );
    assert_eq!(
        target_pid(&entry),
        target_pid(&fx.engine.entry_command(&world, &fx.repo).unwrap())
    );

    fx.engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world() {
    let fx = WorldFixture::new("bridge-port-80");
    let ingress_sock = fx.root.join("http-ingress.sock");
    let listener = UnixListener::bind(&ingress_sock).expect("bind http-ingress.sock");
    fs::set_permissions(&ingress_sock, fs::Permissions::from_mode(0o600)).expect("chmod 0600");
    let world = fx.clone_world(
        WorldTarget::Canonical,
        Some(LocalIngressBridge {
            port: 80,
            host_ingress_sock: ingress_sock.clone(),
        }),
        &fx.snapshot,
    );

    // The app the ingress routes to lives inside the world on a non-privileged port.
    let app = "import socket\n\
         s = socket.socket()\n\
         s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
         s.bind((\"127.0.0.1\", 18081))\n\
         s.listen(8)\n\
         while True:\n\
         \x20   c, _ = s.accept()\n\
         \x20   c.recv(4096)\n\
         \x20   c.sendall(b\"HTTP/1.1 200 OK\\r\\nContent-Length: 9\\r\\n\\r\\nbridge-ok\")\n\
         \x20   c.close()\n";
    let mut spec = service_spec("app", &fx.repo, Some(18081), "");
    spec.runtime = ServiceRuntime::WorldProcess {
        argv: vec!["python3".to_owned(), "-c".to_owned(), app.to_owned()],
        cwd: fx.repo.clone(),
    };
    fx.service(&world, spec);

    // Host ingress stand-in for `slipforkd`: every request that arrives over the AF_UNIX socket is
    // routed back into the world through `dial_world_tcp` (exactly the `ingresses` prefix path).
    let ingress = {
        let engine = Arc::clone(&fx.engine);
        let world = world.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut client) = stream else { continue };
                let engine = engine.clone();
                let world = world.clone();
                thread::spawn(move || {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match client.read(&mut buffer) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let Ok(mut upstream) = engine.dial_world_tcp(&world, 18081) else {
                        return;
                    };
                    if upstream.write_all(&request).is_err() {
                        return;
                    }
                    let mut response = Vec::new();
                    let _ = upstream.read_to_end(&mut response);
                    let _ = client.write_all(&response);
                    let _ = client.shutdown(std::net::Shutdown::Write);
                });
            }
        })
    };
    let _ingress = ingress;

    // (a) Host leg: `slipforkd` itself dials the in-world app through `dial_world_tcp`.
    let mut direct = fx
        .engine
        .dial_world_tcp(&world, 18081)
        .expect("host-side dial into the world");
    direct
        .write_all(b"GET /direct HTTP/1.1\r\nHost: acme.localhost\r\n\r\n")
        .expect("write request");
    let mut direct_reply = Vec::new();
    direct.read_to_end(&mut direct_reply).expect("read reply");
    assert!(
        String::from_utf8_lossy(&direct_reply).contains("bridge-ok"),
        "host leg reply: {}",
        String::from_utf8_lossy(&direct_reply)
    );

    // (b) In-netns leg: `$BASE_URL` (`http://acme.localhost`) resolves to 127.0.0.1:80 inside the
    //     world, which the world init splices to `http-ingress.sock`, which dials back in.
    let out = fx.exec(
        &world,
        "timeout 20 bash -c 'exec 3<>/dev/tcp/127.0.0.1/80; \
         printf \"GET /via-bridge HTTP/1.1\\r\\nHost: acme.localhost\\r\\n\\r\\n\" >&3; \
         cat <&3'",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("bridge-ok"),
        "in-netns :80 bridge reply: {} (stderr: {})",
        out.stdout,
        out.stderr
    );

    fx.engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn world_init_fails_closed_when_the_helper_is_missing() {
    let root = engine_test_dir("missing-world-init");
    let state_dir = root.join("state");
    let (uid, gid) = current_uid_gid();
    let engine = BtrfsEngine::with_driver(
        &state_dir,
        uid,
        gid,
        root.join("no-such-slipfork-world-init"),
        Arc::new(TestTreeDriver::btrfs()) as Arc<dyn SubvolumeDriver>,
    )
    .expect("engine construction");
    let rootfs = support::engine_rootfs(&state_dir);
    let src = root.join("state-src");
    fs::create_dir_all(src.join("state")).expect("state-src");
    fs::write(src.join("state/seed.txt"), b"ancestor\n").expect("seed");
    engine
        .create_snapshot(&src, &snap("base"))
        .expect("snapshot");
    let err = engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs,
            snapshot: snap("base"),
            mounts: vec![],
            base_env: world_env(&root.join("home")),
            local_ingress: None,
        })
        .expect_err("a missing world init helper must fail closed");
    assert!(
        matches!(err, EngineError::WorldInitFailed { .. }),
        "expected WorldInitFailed, got {err:?}"
    );
    assert!(
        !state_dir.join("worlds").exists()
            || fs::read_dir(state_dir.join("worlds"))
                .expect("read_dir worlds")
                .next()
                .is_none(),
        "a failed world boot must leave no half-materialized world behind"
    );
    let _ = fs::remove_dir_all(&root);
}
