//! Protected critical tests for `BtrfsEngine` (`R10`, `R11`, `H-011`).
//!
//! Every suite drives `BtrfsEngine::new` over the real [`slipfork_engine::btrfs::KernelBtrfsDriver`].
//! What stays real is the unprivileged
//! `userns`/`netns`/`ipcns`/`pidns`/`mntns` world PID 1 (`slipfork-world-init`), `nsenter` entry,
//! `pivot_root` mount plumbing, service supervision, and the `AF_UNIX` / `SCM_RIGHTS` descriptor
//! handoff — never a mocked engine and never a fixed sleep as a readiness proof.
//!
//! The real `BTRFS_IOC_*` kernel path (read-only snapshots, writable clones, `--ipc` isolation
//! across parallel containers) is `critical_btrfs_live.rs`, which runs on the Btrfs host.

pub mod support;

mod conformance;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use conformance::assert_world_engine_conformance;
use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver};
use slipfork_engine::{
    BootLineage, BtrfsEngine, EngineError, ExecEnv, InheritedVar, LocalIngressBridge, MountMode,
    ServiceKind, ServiceRuntime, ServiceSpec, ServiceState, SnapshotId, StreamChunk, WorldEngine,
    WorldEntryCommand, WorldHandle, WorldMount, WorldSpec, current_uid_gid,
};
use slipfork_world::WorldTarget;
use support::{
    POLL_INTERVAL, WorldFixture, engine_test_dir, log_text, observe_for, poll_until, world_env,
};

const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Observation window for "a stopped writer never grows its state again" (an absence assertion
/// needs a window; it is never used as a readiness proof).
const QUIET_WINDOW: Duration = Duration::from_millis(150);

/// Bound on the engine's own asynchronous state transitions: `destroy_world` reaping the world
/// PID 1, a service's retained stdout, and a writer's first state write.
const ENGINE_SETTLE_DEADLINE: Duration = Duration::from_secs(10);

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
        kind: ServiceKind::Component,
        runtime: ServiceRuntime::WorldProcess {
            argv: vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: cwd.to_path_buf(),
        },
        env: ExecEnv::default(),
        readiness_port: port,
    }
}

/// `/proc/<pid>/stat` field 5 (`pgrp`) of a host process.
fn host_pgrp(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

/// Every host pid whose process group is `pgid`.
fn group_members(pgid: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| host_pgrp(*pid) == Some(pgid))
        .collect()
}

#[test]
fn r10_a_non_btrfs_state_dir_fails_closed_without_a_silent_fallback() {
    // A real tmpfs (`/dev/shm`), so the refusal contract runs on every Linux host — a Btrfs host
    // included — with zero mocks.
    let root = PathBuf::from(format!(
        "/dev/shm/slipfork-non-btrfs-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let state_dir = root.join("state");
    fs::create_dir_all(&state_dir).expect("create the tmpfs state dir");
    let driver = KernelBtrfsDriver::new();
    let probed = driver
        .probe_facts(&state_dir)
        .expect("statfs the tmpfs state dir");
    assert_ne!(
        probed.fstype,
        BTRFS_SUPER_MAGIC,
        "{} must not be Btrfs for the refusal contract",
        state_dir.display()
    );
    let (uid, gid) = current_uid_gid();
    let err = BtrfsEngine::new(
        &state_dir,
        uid,
        gid,
        env!("CARGO_BIN_EXE_slipfork-world-init"),
        driver,
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
                actual_fstype, probed.fstype,
                "the refusal must report the probed f_type"
            );
            assert_eq!(mount_hint, slipfork_engine::btrfs::mount_hint(&state_dir));
            assert!(
                mount_hint.contains(&state_dir.display().to_string()),
                "the refusal renders the probed path, not a placeholder: {mount_hint}"
            );
            assert!(
                mount_hint.contains("user_subvol_rm_allowed,noatime"),
                "the refusal names the exact mount remediation: {mount_hint}"
            );
        }
        other => panic!("expected BtrfsRequired, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(&state_dir)
            .expect("read the tmpfs state dir")
            .count(),
        0,
        "a refused engine writes nothing into the state dir"
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
            boot: BootLineage::default(),
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
    let fx = WorldFixture::new("layout");
    let driver = KernelBtrfsDriver::new();
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

    // 2. Every world is one writable clone of the same read-only snapshot: the snapshot's own
    //    `state/seed.txt` is readable through each cloned world.
    let snapshot_seed = fx.state_dir.join("snapshots/base/state/seed.txt");
    assert_eq!(
        fs::read(&snapshot_seed).expect("read the sealed snapshot seed"),
        b"ancestor\n",
        "the sealed snapshot carries the fixture seed"
    );
    for world_dir in &after_b {
        assert_eq!(
            fs::read(world_dir.join("state/seed.txt")).expect("read the world's seed"),
            fs::read(&snapshot_seed).expect("re-read the snapshot seed"),
            "{} must read the snapshot's seed through the CoW clone",
            world_dir.display()
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
    assert!(!snapshots.join("sealed").exists());
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn a_rootfs_outside_the_content_addressed_cache_is_refused_before_a_subvolume_exists() {
    let fx = WorldFixture::new("rootfs-cache");
    let driver = KernelBtrfsDriver::new();
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
            boot: BootLineage::default(),
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
            boot: BootLineage::default(),
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
        boot: BootLineage::default(),
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
        boot: BootLineage::default(),
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
            boot: BootLineage::default(),
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
        entry
            .args_prefix
            .iter()
            .any(|arg| arg == &format!("--wdns={}", fx.repo.display()))
            && !entry.args_prefix.iter().any(|arg| arg.starts_with("--wd=")),
        "entry must start the worktree's in-world path at a directory the world carries, opened \
         after the namespace switch: {:?}",
        entry.args_prefix
    );
    assert_eq!(
        entry.cwd,
        Path::new("/"),
        "the spawner chdirs to `/`, which exists on the host and in every world rootfs"
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
    let reaped = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
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
    let captured = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
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
    let produced = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
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
    observe_for(QUIET_WINDOW);
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
fn a_sh_wrapped_component_is_stopped_as_a_process_group_not_only_its_first_child() {
    let fx = WorldFixture::new("process-group");
    let world = fx.world(WorldTarget::Canonical);
    const PORT: u16 = 18123;

    // The production shape `slipforkd` spawns for every component: a distinct `sh -eu -c` parent
    // with the real service underneath it, so the service that owns the port is a grandchild.
    let spec = ServiceSpec {
        id: "api".to_owned(),
        kind: ServiceKind::Component,
        runtime: ServiceRuntime::WorldProcess {
            argv: vec![
                "sh".to_owned(),
                "-eu".to_owned(),
                "-c".to_owned(),
                format!("python3 -m http.server {PORT} --bind 0.0.0.0 & wait"),
            ],
            cwd: fx.repo.clone(),
        },
        env: ExecEnv::default(),
        readiness_port: Some(PORT),
    };
    let api = fx.service(&world, spec);
    let row = fx
        .engine
        .world_processes(&world)
        .expect("rows")
        .into_iter()
        .find(|row| row.id == "api")
        .expect("api row");
    let wrapper = row.pid.expect("the wrapper pid is reported while running");
    let pgid = host_pgrp(wrapper).expect("the wrapper's host process group");
    let children = fs::read_to_string(format!("/proc/{wrapper}/task/{wrapper}/children"))
        .expect("children of the sh wrapper");
    let service_pid: u32 = children
        .split_whitespace()
        .next()
        .expect("the python service under sh")
        .parse()
        .expect("a pid");
    assert_ne!(
        service_pid, wrapper,
        "the defect needs a distinct sh parent and a service grandchild"
    );

    fx.engine.stop_service(&api).expect("stop the component");
    assert_eq!(api.state(), ServiceState::Stopped);

    // Both processes of the group must be gone — the old stop only signalled `sh` and orphaned
    // `python3`, which kept this port bound forever.
    let reaped = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
        if Path::new(&format!("/proc/{wrapper}")).exists()
            || Path::new(&format!("/proc/{service_pid}")).exists()
            || !group_members(pgid).is_empty()
        {
            Err(format!("group {pgid} still has {:?}", group_members(pgid)))
        } else {
            Ok(())
        }
    });
    assert!(
        reaped.is_ok(),
        "stop must reap the whole process group: {reaped:?}"
    );
    fx.engine
        .dial_world_tcp(&world, PORT)
        .expect_err("the stopped component must no longer own its port");

    fx.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn stop_reaps_a_group_descendant_that_outlives_its_supervisor() {
    let fx = WorldFixture::new("process-group-orphan");
    let world = fx.world(WorldTarget::Canonical);
    const PORT: u16 = 18124;

    // `sh` backgrounds the server and exits: the watcher flips the row to `Exited` while the
    // server keeps the port. Stop must still reach that descendant through the group.
    let spec = ServiceSpec {
        id: "orphan".to_owned(),
        kind: ServiceKind::Component,
        runtime: ServiceRuntime::WorldProcess {
            argv: vec![
                "sh".to_owned(),
                "-eu".to_owned(),
                "-c".to_owned(),
                format!(
                    "python3 -m http.server {PORT} --bind 0.0.0.0 & \
                     while [ ! -f exit-now ]; do sleep 0.05; done"
                ),
            ],
            cwd: fx.repo.clone(),
        },
        env: ExecEnv::default(),
        readiness_port: Some(PORT),
    };
    let orphan = fx.service(&world, spec);
    let wrapper = fx
        .engine
        .world_processes(&world)
        .expect("rows")
        .into_iter()
        .find(|row| row.id == "orphan")
        .and_then(|row| row.pid)
        .expect("the wrapper pid is reported while it lives");
    let pgid = host_pgrp(wrapper).expect("the wrapper's host process group");

    // The supervised child exits on its own; the service it forked survives in the group.
    fs::write(fx.repo.join("exit-now"), b"exit\n").expect("ask the wrapper to exit");
    let exited = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
        let state = fx
            .engine
            .world_processes(&world)
            .expect("rows")
            .into_iter()
            .find(|row| row.id == "orphan")
            .map(|row| row.state)
            .expect("orphan row");
        if state == ServiceState::Exited {
            Ok(())
        } else {
            Err(format!("row is {state:?}"))
        }
    });
    assert!(exited.is_ok(), "the wrapper must have exited: {exited:?}");
    let survivors = group_members(pgid);
    assert!(
        !survivors.is_empty(),
        "the forked server must outlive the wrapper in group {pgid}: {survivors:?}"
    );

    fx.engine
        .stop_service(&orphan)
        .expect("stop must reap the orphaned group");
    assert!(
        group_members(pgid).is_empty(),
        "a descendant that outlived its supervisor must still be reaped: {:?}",
        group_members(pgid)
    );
    fx.engine
        .dial_world_tcp(&world, PORT)
        .expect_err("the reaped orphan must no longer own its port");

    // The same `Exited`/surviving-descendant shape under the world-wide stop path: `freeze_world`
    // stops every recorded row, including this one, so the seal can never capture a live writer.
    const FREEZE_PORT: u16 = 18125;
    let freezer = fx.service(
        &world,
        ServiceSpec {
            id: "freezer".to_owned(),
            kind: ServiceKind::Component,
            runtime: ServiceRuntime::WorldProcess {
                argv: vec![
                    "sh".to_owned(),
                    "-eu".to_owned(),
                    "-c".to_owned(),
                    format!(
                        "python3 -m http.server {FREEZE_PORT} --bind 0.0.0.0 & \
                         while [ ! -f exit-freeze ]; do sleep 0.05; done"
                    ),
                ],
                cwd: fx.repo.clone(),
            },
            env: ExecEnv::default(),
            readiness_port: Some(FREEZE_PORT),
        },
    );
    let freezer_wrapper = fx
        .engine
        .world_processes(&world)
        .expect("rows")
        .into_iter()
        .find(|row| row.id == "freezer")
        .and_then(|row| row.pid)
        .expect("the freezer wrapper pid is reported while it lives");
    let freezer_pgid = host_pgrp(freezer_wrapper).expect("the freezer wrapper's process group");
    fs::write(fx.repo.join("exit-freeze"), b"exit\n").expect("ask the freezer wrapper to exit");
    let freezer_exited = poll_until(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, || {
        let state = fx
            .engine
            .world_processes(&world)
            .expect("rows")
            .into_iter()
            .find(|row| row.id == "freezer")
            .map(|row| row.state)
            .expect("freezer row");
        if state == ServiceState::Exited {
            Ok(())
        } else {
            Err(format!("freezer row is {state:?}"))
        }
    });
    assert!(
        freezer_exited.is_ok(),
        "the freezer wrapper must have exited: {freezer_exited:?}"
    );
    assert!(
        !group_members(freezer_pgid).is_empty(),
        "the forked freezer must outlive the wrapper in group {freezer_pgid}: {:?}",
        group_members(freezer_pgid)
    );

    fx.engine
        .freeze_world(&world, &snap("sealed-over-orphan"))
        .expect("freeze must stop every recorded row before it seals");
    assert_eq!(
        freezer.state(),
        ServiceState::Stopped,
        "the world-wide stop covers an already-`Exited` row"
    );
    assert!(
        group_members(freezer_pgid).is_empty(),
        "freeze must reap the descendant that outlived its supervisor: {:?}",
        group_members(freezer_pgid)
    );
    fx.engine
        .dial_world_tcp(&world, FREEZE_PORT)
        .expect_err("a frozen world must have no live writer on its port");

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
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stdout.starts_with(&format!("entered|{uid}|")),
        "entry must preserve the host uid: {stdout}"
    );
    assert!(
        !stderr.contains("getcwd"),
        "the entry shell's first getcwd(2) must succeed in-world: {stderr}"
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
fn entry_and_exec_start_on_the_in_world_directory_mapped_from_the_host_path() {
    let fx = WorldFixture::new("in-world-cwd");
    let sub = fx.repo.join("pkg");
    fs::create_dir_all(&sub).expect("repo subdir");
    // A guest path that has no host counterpart at all: only the in-world mapping can start here.
    let guest = fx.root.join("stage");
    assert!(
        !guest.exists(),
        "the fixture must not create the in-world mount target on the host"
    );
    let world = fx
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: fx.rootfs.clone(),
            snapshot: fx.snapshot.clone(),
            mounts: vec![WorldMount {
                host_path: fx.repo.clone(),
                guest_path: guest.clone(),
                mode: MountMode::ReadWrite,
            }],
            base_env: world_env(&fx.home),
            local_ingress: None,
            boot: BootLineage::default(),
        })
        .expect("clone world with a remapped worktree");
    let world_cwd = guest.join("pkg");

    // 1. The argv names the in-world directory, never the host pathname.
    let entry = fx.engine.entry_command(&world, &sub).expect("entry");
    assert_eq!(entry.cwd, Path::new("/"));
    assert!(
        entry
            .args_prefix
            .iter()
            .any(|arg| arg == &format!("--wdns={}", world_cwd.display())),
        "entry must open the mapped in-world directory after setns: {:?}",
        entry.args_prefix
    );
    assert!(
        !entry
            .args_prefix
            .iter()
            .any(|arg| arg.starts_with("--wd=") || arg.contains(sub.display().to_string().as_str())),
        "the caller's host path must never appear in the entry argv: {:?}",
        entry.args_prefix
    );

    // 2. Driving it exactly like the thin CLI does (spawn cwd `/`, argv untouched): the shell's
    //    first `getcwd(2)` succeeds and it really starts in the mapped directory.
    let out = Command::new(&entry.program)
        .args(&entry.args_prefix)
        .args(["sh", "-c", "pwd"])
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
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("getcwd"),
        "the entry shell must start on a reachable world dentry: {stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        world_cwd.display().to_string(),
        "the entry shell must run in the world's mapped directory"
    );

    // 3. The same builder backs non-interactive exec: no wrapper shell, same in-world directory.
    let exec = fx
        .engine
        .exec_in_world(
            &world,
            &["sh".to_owned(), "-c".to_owned(), "pwd".to_owned()],
            &sub,
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("exec in world");
    assert_eq!(exec.exit_code, 0, "stderr: {}", exec.stderr);
    assert_eq!(
        exec.stdout.trim(),
        world_cwd.display().to_string(),
        "exec must start in the same mapped in-world directory"
    );
    assert!(
        !exec.stderr.contains("getcwd"),
        "exec must never inherit an unreachable cwd: {}",
        exec.stderr
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
    let engine = support::require_engine(&state_dir, root.join("no-such-slipfork-world-init"));
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
            boot: BootLineage::default(),
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

/// The exact spec a `slipfork restart` hands `WorldEngine::reload_world`.
fn reload_spec(fx: &WorldFixture, snapshot: SnapshotId) -> WorldSpec {
    WorldSpec {
        target: WorldTarget::Canonical,
        rootfs: fx.rootfs.clone(),
        snapshot,
        mounts: vec![WorldMount {
            host_path: fx.repo.clone(),
            guest_path: fx.repo.clone(),
            mode: MountMode::ReadWrite,
        }],
        base_env: world_env(&fx.home),
        local_ingress: None,
        boot: BootLineage::default(),
    }
}

#[test]
fn a_frozen_world_refuses_a_reload_over_its_read_only_subvolume() {
    let fx = WorldFixture::new("reload-frozen");
    let world = fx.world(WorldTarget::Canonical);
    fx.engine
        .freeze_world(&world, &snap("reload-sealed"))
        .expect("freeze the world");

    let spec = reload_spec(&fx, fx.snapshot.clone());
    let err = fx
        .engine
        .reload_world(&world, &spec)
        .expect_err("a frozen world must not be re-rooted");
    assert!(
        matches!(err, EngineError::WorldNotReloadable { .. }),
        "expected WorldNotReloadable, got {err:?}"
    );
    assert!(
        fx.engine.world_lineage(&world).is_ok(),
        "the refusal leaves the frozen record intact"
    );

    fx.engine
        .destroy_world(world)
        .expect("destroy the frozen world");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn a_reload_naming_a_pruned_snapshot_fails_before_it_touches_the_world() {
    let fx = WorldFixture::new("reload-pruned");
    let world = fx.world(WorldTarget::Canonical);
    let wrote = fx.exec(&world, "printf kept > \"$SLIPFORK_WORLD_STATE/keep.txt\"");
    assert_eq!(wrote.exit_code, 0, "stderr: {}", wrote.stderr);

    let spec = reload_spec(&fx, snap("pruned-base"));
    let err = fx
        .engine
        .reload_world(&world, &spec)
        .expect_err("a pruned snapshot must fail closed");
    assert!(
        matches!(err, EngineError::SnapshotNotFound(_)),
        "expected SnapshotNotFound, got {err:?}"
    );

    // The shared pre-flight ran before any mutation: the world's PID 1, subvolume, and state
    // directory still stand, so a mistyped restart never destroys a world's backing-service state.
    let still_there = fx.exec(&world, "cat \"$SLIPFORK_WORLD_STATE/keep.txt\"");
    assert_eq!(still_there.exit_code, 0, "stderr: {}", still_there.stderr);
    assert_eq!(still_there.stdout.trim(), "kept");

    // A malformed spec lacking a world-scoped `HOME` is likewise caught by `world_preflight`
    // before stopping services or re-cloning over a new base snapshot.
    let src_v2 = fx.root.join("state-src-v2");
    fs::create_dir_all(src_v2.join("state")).expect("state-src-v2");
    fs::write(src_v2.join("state/seed.txt"), b"v2\n").expect("seed-v2");
    fx.engine
        .create_snapshot(&src_v2, &snap("base-v2"))
        .expect("snapshot base-v2");
    let mut no_home = reload_spec(&fx, snap("base-v2"));
    no_home.base_env = ExecEnv::default();
    let err = fx
        .engine
        .reload_world(&world, &no_home)
        .expect_err("a reload spec without world HOME must fail closed in pre-flight");
    assert!(
        matches!(err, EngineError::InvalidWorldSpec { .. }),
        "expected InvalidWorldSpec, got {err:?}"
    );
    let untouched = fx.exec(&world, "cat \"$SLIPFORK_WORLD_STATE/keep.txt\"");
    assert_eq!(untouched.exit_code, 0, "stderr: {}", untouched.stderr);
    assert_eq!(untouched.stdout.trim(), "kept");

    fx.engine.destroy_world(world).expect("destroy the world");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn reload_across_a_new_base_replaces_the_subvolume_and_never_lies_after_a_failed_boot() {
    let fx = WorldFixture::new("reload-base-change");
    let world = fx.world(WorldTarget::Canonical);
    let _svc = fx.service(&world, service_spec("bg", &fx.repo, None, "sleep 30"));
    assert_eq!(
        fx.exec(&world, "printf old > \"$SLIPFORK_WORLD_STATE/old.txt\"")
            .exit_code,
        0
    );

    let src_v2 = fx.root.join("state-src-v2");
    fs::create_dir_all(src_v2.join("state")).expect("state-src-v2");
    fs::write(src_v2.join("state/seed.txt"), b"v2\n").expect("seed-v2");
    fx.engine
        .create_snapshot(&src_v2, &snap("base-v2"))
        .expect("snapshot base-v2");

    // 1. Destructive `!same_base` reload whose boot fails after re-cloning `base-v2`:
    //    `local_ingress` names an unbound socket, failing inside `boot_world` with
    //    `WorldInitFailed`. The record transitions to `Failed` (`WorldNotLive`) and keeps the
    //    truthful `base-v2` storage fact instead of rolling back to `fx.snapshot`.
    let mut bad_boot = reload_spec(&fx, snap("base-v2"));
    bad_boot.local_ingress = Some(LocalIngressBridge {
        port: 80,
        host_ingress_sock: fx.root.join("missing-ingress.sock"),
    });
    let err = fx
        .engine
        .reload_world(&world, &bad_boot)
        .expect_err("missing ingress socket must fail boot_world");
    assert!(
        matches!(err, EngineError::WorldInitFailed { .. }),
        "expected WorldInitFailed, got {err:?}"
    );
    assert!(
        matches!(
            fx.engine.world_processes(&world),
            Err(EngineError::WorldNotLive { .. })
        ),
        "live operations on a failed reload must return WorldNotLive"
    );

    // 2. Retrying back onto the original `fx.snapshot` proves the record did NOT lie and claim
    //    `fx.snapshot` over `base-v2`: it must re-clone `fx.snapshot` and serve `"ancestor"`,
    //    whereas a rollback lie would have treated `same_base` as true and kept `"v2"`.
    fx.engine
        .reload_world(&world, &reload_spec(&fx, fx.snapshot.clone()))
        .expect("retry reload back to original base");
    assert!(
        fx.engine
            .world_processes(&world)
            .expect("live after retry")
            .is_empty(),
        "reload clears old service rows"
    );
    assert_eq!(
        fx.exec(&world, "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"")
            .stdout
            .trim(),
        "ancestor"
    );
    assert_ne!(
        fx.exec(&world, "test -e \"$SLIPFORK_WORLD_STATE/old.txt\"")
            .exit_code,
        0,
        "re-cloning replaced the previous subvolume state"
    );

    // 3. Happy-path `!same_base` reload onto `base-v2` replaces the subvolume cleanly, and a
    //    subsequent failed reload remains destroyable via `destroy_world`.
    fx.engine
        .reload_world(&world, &reload_spec(&fx, snap("base-v2")))
        .expect("reload onto base-v2");
    assert_eq!(
        fx.exec(&world, "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"")
            .stdout
            .trim(),
        "v2"
    );
    let err = fx
        .engine
        .reload_world(&world, &bad_boot)
        .expect_err("second failed boot");
    assert!(matches!(err, EngineError::WorldInitFailed { .. }));
    fx.engine
        .destroy_world(world)
        .expect("destroy_world must release a Failed world");
    let _ = fs::remove_dir_all(&fx.root);
}
