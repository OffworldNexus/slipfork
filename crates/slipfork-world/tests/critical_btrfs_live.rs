//! Protected live Btrfs suite for the `WorldEngine` port (`--features live`).
//!
//! Every assertion runs against real Btrfs subvolumes on disk; nothing here is mocked. On a host
//! whose scratch root is not Btrfs the suite FAILS CLOSED with the mount remedy instead of
//! skipping, because a skipped CoW suite hides broken snapshots forever.

use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BtrfsEngine, Dirtiness, Error, ForkName, PostgresLauncher, PostgresTemplate, StoreLayout,
    WorldEngine, WorldName, is_btrfs_path, is_subvolume_path,
};

/// Two kernel snapshots plus path bookkeeping: measured ~0.6ms on local NVMe, ~15–30ms on a raw
/// cloud volume, ~110–195ms loop-backed on CI (each `SNAP_CREATE_V2` commits through the loop device).
const FORK_BUDGET: Duration = Duration::from_millis(350);

const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .expect("fail closed: HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>");
    PathBuf::from(home).join(".cache/slipfork/tests")
}

fn live_run_root() -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory such as ~/.cache/slipfork/tests or /var/tmp/slipfork",
        scratch.display()
    );
    fs::create_dir_all(&scratch).expect("create Btrfs scratch root");
    match is_btrfs_path(&scratch) {
        Ok(true) => {}
        Ok(false) => panic!(
            "fail closed: `{}` is not on Btrfs (BTRFS_SUPER_MAGIC 0x9123683E missing).\nnext: run on a Btrfs host or export SLIPFORK_TEST_ROOT=<btrfs dir>; {MOUNT_REMEDY}",
            scratch.display()
        ),
        Err(err) => panic!(
            "fail closed: probing `{}` failed: {err}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("world-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create run root");
    run
}

/// The fork contract's checkpoint precondition: flush the scratch fs, then time only snapshots.
fn flush_scratch(root: &Path) {
    let dir = fs::File::open(root).expect("fail closed: open the scratch root to flush it");
    // SAFETY: `dir` is a live directory descriptor and `syncfs` only flushes that one filesystem.
    let flushed = unsafe { libc::syncfs(dir.as_raw_fd()) };
    assert_eq!(flushed, 0, "fail closed: syncfs on the scratch root failed");
}

fn subvolumes_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)
            .expect("fail closed: read the scratch tree while scanning for leftover subvolumes");
        for entry in entries {
            let entry = entry.expect("fail closed: read a scratch tree entry");
            let file_type = entry
                .file_type()
                .expect("fail closed: stat a scratch tree entry");
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            if is_subvolume_path(&path).expect("fail closed: probe a scratch tree directory") {
                found.push(path.clone());
            }
            stack.push(path);
        }
    }
    found
}

#[test]
fn btrfs_lifecycle() {
    let run = live_run_root();
    let engine = BtrfsEngine::new(StoreLayout::at(run.join(".slipfork-store")));

    // 1. BTRFS_SUPER_MAGIC probe already passed; the store is ours to create.
    engine.ensure_store().expect("create .slipfork-store");
    assert!(
        engine.layout().images_root().is_dir(),
        "images/ must be owned"
    );
    assert!(
        engine.layout().worlds_root().is_dir(),
        "worlds/ must be owned"
    );

    // 1b. A plain (non-subvolume) checkout is seeded here for 3b's copy proof.
    let plain = run.join("plaincheck");
    fs::create_dir_all(&plain).expect("seed a plain checkout");
    fs::write(plain.join("Forkfile"), "# forkfile: 0.1\n").expect("seed Forkfile");

    // 2. A subvolume checkout keeps the `SNAP_CREATE_V2` fast path; seed the demo tree in place.
    let project = run.join("demo");
    engine
        .create_subvolume(&project)
        .expect("create the demo subvolume");
    fs::create_dir_all(project.join("src")).expect("seed project tree");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("seed source");
    fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("seed manifest");
    fs::create_dir_all(project.join("src/nested")).expect("seed nested tree");
    fs::write(project.join("src/nested/main.rs"), "pub fn nested() {}\n")
        .expect("seed nested file");
    fs::set_permissions(
        project.join("src/nested"),
        fs::Permissions::from_mode(0o750),
    )
    .expect("pin the nested directory mode");
    fs::create_dir_all(project.join(".git/objects")).expect("seed git metadata");
    fs::write(project.join(".git/HEAD"), "ref: refs/heads/develop\n").expect("seed git head");
    symlink("src/nested/main.rs", project.join("link")).expect("seed relative symlink");
    fs::write(project.join("run.sh"), "#!/bin/sh\necho demo\n").expect("seed script");
    fs::set_permissions(project.join("run.sh"), fs::Permissions::from_mode(0o755))
        .expect("pin the executable mode");
    assert!(is_subvolume_path(&project).expect("probe the demo subvolume"));

    // 3. World data subvolume under worlds/.
    let world = WorldName::parse("demo").expect("valid world name");
    let world_data = engine.create_world(&world).expect("create world data");
    assert!(is_subvolume_path(&world_data).expect("probe world data"));
    assert_eq!(
        engine.create_world(&world).expect("idempotent world"),
        world_data
    );

    // 3b. `fork` on a plain checkout copies the tree into `<project>/.forks/<name>` and leaves the
    // parent inode untouched; the copy is a plain directory, never a subvolume.
    let dir_inode = fs::metadata(&plain).expect("stat the plain checkout").ino();
    fs::create_dir_all(plain.join("src/nested")).expect("seed the plain tree");
    fs::write(plain.join("src/main.rs"), "fn main() {}\n").expect("seed plain source");
    fs::write(plain.join("src/nested/main.rs"), "pub fn nested() {}\n")
        .expect("seed the plain nested file");
    fs::set_permissions(plain.join("src/nested"), fs::Permissions::from_mode(0o750))
        .expect("pin the plain nested mode");
    fs::write(plain.join("run.sh"), "#!/bin/sh\necho demo\n").expect("seed the plain script");
    fs::set_permissions(plain.join("run.sh"), fs::Permissions::from_mode(0o755))
        .expect("pin the plain executable mode");
    symlink("src/nested/main.rs", plain.join("link")).expect("seed the plain symlink");
    fs::create_dir_all(plain.join(".forks/stale")).expect("seed a stale fork tree");
    let copy = ForkName::parse("feat-copy").expect("valid fork name");
    let copied = engine
        .fork(&plain, &world, &copy)
        .expect("fork a plain checkout");
    assert_eq!(
        fs::metadata(&plain)
            .expect("restat the plain checkout")
            .ino(),
        dir_inode,
        "fork must never swap the parent checkout inode"
    );
    assert_eq!(
        fs::read_to_string(copied.project.join("src/main.rs")).expect("read the copied source"),
        "fn main() {}\n"
    );
    assert_eq!(
        fs::read_to_string(copied.project.join("src/nested/main.rs")).expect("read nested copy"),
        "pub fn nested() {}\n"
    );
    assert_eq!(
        fs::metadata(copied.project.join("src/nested"))
            .expect("probe the copied nested directory")
            .permissions()
            .mode()
            & 0o777,
        0o750,
        "copy_checkout must carry directory modes"
    );
    assert_eq!(
        fs::metadata(copied.project.join("run.sh"))
            .expect("probe the copied script")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "copy_checkout must preserve executable modes"
    );
    assert!(
        fs::symlink_metadata(copied.project.join("link"))
            .expect("probe the copied symlink")
            .file_type()
            .is_symlink(),
        "copy_checkout must recreate symlinks, never follow them"
    );
    assert_eq!(
        fs::read_link(copied.project.join("link")).expect("read the copied symlink"),
        Path::new("src/nested/main.rs")
    );
    assert!(
        !copied.project.join(".forks").exists(),
        "copy_checkout must never copy `.forks` into the fork"
    );
    assert!(
        !is_subvolume_path(&copied.project).expect("probe the copied checkout"),
        "a copied checkout is a plain directory, not a subvolume"
    );
    assert!(
        is_subvolume_path(&copied.world).expect("probe the world snapshot"),
        "the world snapshot is still a real subvolume"
    );
    engine
        .drop_fork(&plain, &world, &copy, Dirtiness::Clean, false)
        .expect("drop the copied fork");
    assert!(
        !copied.project.exists(),
        "drop must remove the copied checkout directory"
    );
    assert!(
        !copied.world.exists(),
        "drop must remove the paired world snapshot"
    );

    // 4. Steady-state CoW fork of the project AND the world data subvolume, measured over three
    // cycles: the promise is the latency of a running world, not the first transaction commit.
    // Every cycle starts with the spec's checkpoint flush of active writers; only the two
    // snapshot ioctls inside `fork` are timed.
    let warm = ForkName::parse("feat-warm").expect("valid fork name");
    flush_scratch(&run);
    engine
        .fork(&project, &world, &warm)
        .expect("warm up the snapshot path");
    engine
        .drop_fork(&project, &world, &warm, Dirtiness::Clean, false)
        .expect("drop the warm-up fork");

    let mut timings: Vec<(&str, Duration)> = Vec::new();
    for name in ["feat-a", "feat-b", "feat-c"] {
        let fork = ForkName::parse(name).expect("valid fork name");
        flush_scratch(&run);
        let started = Instant::now();
        let forked = engine
            .fork(&project, &world, &fork)
            .expect("fork project + world");
        let elapsed = started.elapsed();
        assert_eq!(forked.project, project.join(".forks").join(name));
        assert_eq!(
            forked.world,
            run.join(format!(".slipfork-store/worlds/{name}--demo"))
        );
        assert!(is_subvolume_path(&forked.project).expect("probe project snapshot"));
        assert!(is_subvolume_path(&forked.world).expect("probe world snapshot"));
        if name == "feat-a" {
            // CoW independence, proven once: a write in the snapshot never reaches the parent.
            fs::write(
                forked.project.join("src/main.rs"),
                "fn main() { fork(); }\n",
            )
            .expect("write inside the fork");
            assert_eq!(
                fs::read_to_string(project.join("src/main.rs")).expect("read parent payload"),
                "fn main() {}\n",
                "a fork must be a CoW clone, never a shared writable tree"
            );
        }
        engine
            .drop_fork(&project, &world, &fork, Dirtiness::Clean, false)
            .expect("drop the measured fork pair");
        assert!(
            !forked.project.exists() && !forked.world.exists(),
            "the measured fork pair must be reaped"
        );
        eprintln!("fork {name}: {elapsed:?}");
        timings.push((name, elapsed));
    }
    let best = timings
        .iter()
        .map(|(_, elapsed)| *elapsed)
        .min()
        .expect("three measured fork cycles");
    assert!(
        best < FORK_BUDGET,
        "steady-state CoW fork of project + world took {best:?} at best ({timings:?}), budget {FORK_BUDGET:?}"
    );

    // 5. A dirty checkout refuses the drop before touching either snapshot.
    let dirty = ForkName::parse("feat-dirty").expect("valid fork name");
    let dirty_fork = engine
        .fork(&project, &world, &dirty)
        .expect("fork the dirty-checkout pair");
    assert!(
        matches!(
            engine.drop_fork(&project, &world, &dirty, Dirtiness::Dirty, false),
            Err(Error::RefusedDirtyCheckout { .. })
        ),
        "a dirty fork must be refused without --force"
    );
    assert!(is_subvolume_path(&dirty_fork.project).expect("project snapshot survives refusal"));
    assert!(is_subvolume_path(&dirty_fork.world).expect("world snapshot survives refusal"));
    engine
        .drop_fork(&project, &world, &dirty, Dirtiness::Dirty, true)
        .expect("--force drops the fork");
    assert!(
        !dirty_fork.project.exists(),
        "forced drop removes the project snapshot"
    );
    assert!(
        !dirty_fork.world.exists(),
        "forced drop removes the paired world snapshot"
    );

    // 6. Real kernel seal: BTRFS_SUBVOL_RDONLY, never a 0o555 mode bit.
    let sealing = ForkName::parse("feat-seal").expect("valid fork name");
    let sealed = engine
        .fork(&project, &world, &sealing)
        .expect("fork for sealing");
    let payload = sealed.world.join("PG_VERSION");
    fs::write(&payload, "18\n").expect("seed world payload");
    engine.seal(&sealed.world).expect("seal world snapshot");
    assert!(engine.is_sealed(&sealed.world).expect("probe seal"));
    let denied = fs::write(sealed.world.join("intruder"), b"x")
        .expect_err("sealed subvolume rejects new entries");
    assert_eq!(
        denied.raw_os_error(),
        Some(libc::EROFS),
        "expected EROFS on sealed write, got {denied:?}"
    );
    let removal = fs::remove_file(&payload).expect_err("sealed subvolume rejects payload removal");
    assert_eq!(
        removal.raw_os_error(),
        Some(libc::EROFS),
        "expected EROFS on sealed removal, got {removal:?}"
    );

    // 7. Destroy of a sealed subvolume clears BTRFS_SUBVOL_RDONLY first, then succeeds.
    engine
        .destroy(&sealed.world, Dirtiness::Clean, false)
        .expect("destroy sealed world snapshot");
    assert!(
        !sealed.world.exists(),
        "sealed subvolume must be destroyed after unsealing"
    );
    engine
        .drop_fork(&project, &world, &sealing, Dirtiness::Clean, false)
        .expect("drop the sealed fork pair");
    assert!(
        !sealed.project.exists(),
        "fork cleanup must not leak the project snapshot"
    );

    // 7b. Golden postgres-18 template and its flat-sibling PGDATA clone, on real Btrfs. The real
    // `initdb` runs in the protected pod suite (`critical_pod_runtime`); here the seal, the
    // `< 1ms` snapshot, the flat sibling layout, and the stale-lock strip are the subject.
    let store = StoreLayout::at(run.join(".slipfork-store"));
    let template = PostgresTemplate::new(store.clone());
    fs::create_dir_all(template.root()).expect("create templates/");
    engine
        .create_subvolume(&template.path())
        .expect("create the golden cluster subvolume");
    fs::create_dir_all(template.path().join("base/1")).expect("seed the cluster tree");
    fs::write(template.path().join("PG_VERSION"), "18\n").expect("seed PG_VERSION");
    fs::write(template.path().join("postmaster.pid"), "4242\n").expect("seed a stale lock");
    engine
        .seal(&template.path())
        .expect("seal the golden template");
    assert!(engine.is_sealed(&template.path()).expect("probe the seal"));
    let denied = fs::write(template.path().join("intruder"), b"x")
        .expect_err("a sealed template must refuse writes");
    assert_eq!(denied.raw_os_error(), Some(libc::EROFS), "{denied:?}");

    let launcher = PostgresLauncher::new(store.clone(), world.clone());
    let warm = launcher
        .clone(&engine, &template.path(), "warm")
        .expect("warm the snapshot path");
    engine
        .destroy(&warm.path, Dirtiness::Clean, true)
        .expect("destroy the warm clone");
    let pgdata = launcher
        .clone(&engine, &template.path(), "boot")
        .expect("clone PGDATA for the world");
    let worlds_root = run.join(".slipfork-store/worlds");
    assert_eq!(
        pgdata.path,
        worlds_root.join("demo.pg.boot"),
        "PGDATA must be a flat sibling of the world, never nested inside it"
    );
    assert_eq!(
        pgdata.path.parent(),
        Some(worlds_root.as_path()),
        "the clone must never land inside the world subvolume"
    );
    // One `SNAP_CREATE_V2` ioctl: ~0.3ms local NVMe, 8.6–27.2ms raw cloud volume, 60–110ms on
    // loop-backed CI Btrfs. The ceiling catches added user-space work (a second ioctl, a copy, a
    // syncfs) or an `initdb` re-run (> 1s), never block-device commit latency.
    assert!(
        pgdata.elapsed < Duration::from_millis(250),
        "PGDATA clone took {:?}, budget 250ms (one kernel snapshot ioctl)",
        pgdata.elapsed
    );
    assert!(
        pgdata.path.join("PG_VERSION").is_file(),
        "the clone must carry the sealed cluster payload"
    );
    assert!(
        !pgdata.path.join("postmaster.pid").exists(),
        "a clone must never inherit postmaster.pid from its parent"
    );
    fs::write(pgdata.path.join("postmaster.pid"), "4242\n").expect("plant a stale lock");
    let reused = launcher
        .clone(&engine, &template.path(), "boot")
        .expect("reuse the existing clone");
    assert_eq!(
        reused.elapsed,
        Duration::ZERO,
        "a reboot must not pay a second snapshot"
    );
    assert!(
        !pgdata.path.join("postmaster.pid").exists(),
        "a reused PGDATA must strip the stale lock too"
    );

    // 8. Dropping a world sweeps `<fork>--<world>` snapshots and the data subvolume.
    let keeping = ForkName::parse("feat-keep").expect("valid fork name");
    let kept = engine
        .fork(&project, &world, &keeping)
        .expect("fork a world child");
    engine.drop_world(&world).expect("drop the world");
    assert!(
        !run.join(".slipfork-store/worlds/feat-keep--demo").exists(),
        "world drop must sweep `<fork>--<world>` snapshots"
    );
    assert!(
        !pgdata.path.exists(),
        "world drop must sweep the flat `<world>.pg.<pod>` PGDATA clones"
    );
    engine
        .destroy(&kept.project, Dirtiness::Clean, false)
        .expect("destroy the leftover project snapshot");

    // 9. Full teardown: zero leftover subvolumes under the scratch run.
    engine
        .destroy(&project, Dirtiness::Clean, false)
        .expect("destroy the project subvolume");
    // The store-global golden template survives every world drop by design; this scratch store
    // owns it, so the leftover proof must reap it explicitly.
    engine
        .destroy(&template.path(), Dirtiness::Clean, true)
        .expect("destroy the sealed golden template");
    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove scratch run");
}
