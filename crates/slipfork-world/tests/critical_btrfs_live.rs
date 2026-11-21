//! Protected live Btrfs suite for the `WorldEngine` port (`--features live`).
//!
//! Every assertion runs against real Btrfs subvolumes on disk; nothing here is mocked. On a host
//! whose scratch root is not Btrfs the suite FAILS CLOSED with the mount remedy instead of
//! skipping, because a skipped CoW suite hides broken snapshots forever.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BtrfsEngine, Dirtiness, Error, ForkName, StoreLayout, WorldEngine, WorldName, is_btrfs_path,
    is_subvolume_path,
};

/// Two kernel snapshots plus path bookkeeping must stay well under this budget.
const FORK_BUDGET: Duration = Duration::from_millis(50);

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
    let engine = BtrfsEngine::new(StoreLayout::new(&run));

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

    // 2. Plain checkout -> in-place subvolume conversion with zero copying.
    let project = run.join("demo");
    fs::create_dir_all(project.join("src")).expect("seed project tree");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("seed source");
    fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("seed manifest");
    assert!(!is_subvolume_path(&project).expect("probe plain directory"));
    engine
        .adopt_project(&project)
        .expect("convert the checkout in place");
    assert!(is_subvolume_path(&project).expect("probe converted checkout"));
    assert_eq!(
        fs::read_to_string(project.join("src/main.rs")).expect("read converted payload"),
        "fn main() {}\n",
        "in-place conversion must not lose file contents"
    );
    engine
        .adopt_project(&project)
        .expect("conversion must be idempotent");

    // 3. World data subvolume under worlds/.
    let world = WorldName::parse("demo").expect("valid world name");
    let world_data = engine.create_world(&world).expect("create world data");
    assert!(is_subvolume_path(&world_data).expect("probe world data"));
    assert_eq!(
        engine.create_world(&world).expect("idempotent world"),
        world_data
    );

    // 4. Sub-50ms CoW fork of the project AND the world data subvolume.
    let fork = ForkName::parse("feat-auth").expect("valid fork name");
    let started = Instant::now();
    let forked = engine
        .fork(&project, &world, &fork)
        .expect("fork project + world");
    let elapsed = started.elapsed();
    assert!(
        elapsed < FORK_BUDGET,
        "CoW fork of project + world took {elapsed:?}, budget {FORK_BUDGET:?}"
    );
    assert_eq!(forked.project, project.join(".forks/feat-auth"));
    assert_eq!(
        forked.world,
        run.join(".slipfork-store/worlds/feat-auth--demo")
    );
    assert!(is_subvolume_path(&forked.project).expect("probe project snapshot"));
    assert!(is_subvolume_path(&forked.world).expect("probe world snapshot"));
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

    // 5. A dirty checkout refuses the drop before touching either snapshot.
    assert!(
        matches!(
            engine.drop_fork(&project, &world, &fork, Dirtiness::Dirty, false),
            Err(Error::RefusedDirtyCheckout { .. })
        ),
        "a dirty fork must be refused without --force"
    );
    assert!(is_subvolume_path(&forked.project).expect("project snapshot survives refusal"));
    assert!(is_subvolume_path(&forked.world).expect("world snapshot survives refusal"));
    engine
        .drop_fork(&project, &world, &fork, Dirtiness::Dirty, true)
        .expect("--force drops the fork");
    assert!(
        !forked.project.exists(),
        "forced drop removes the project snapshot"
    );
    assert!(
        !forked.world.exists(),
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
    engine
        .destroy(&kept.project, Dirtiness::Clean, false)
        .expect("destroy the leftover project snapshot");

    // 9. Full teardown: zero leftover subvolumes under the scratch run.
    engine
        .destroy(&project, Dirtiness::Clean, false)
        .expect("destroy the project subvolume");
    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove scratch run");
}
