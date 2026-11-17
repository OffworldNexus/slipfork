//! Shared [`WorldEngine`] conformance checks executed against every world engine adapter.
//!
//! The suite asserts only what every engine must satisfy *without* a live world: honest
//! fail-closed error types, mount-spec validation, environment redaction, opaque handle stability,
//! and seal-once freeze semantics. Real CoW lineage (frozen snapshot → independent sibling
//! clones, `$UID` ownership, real bind mounts) is asserted by `tests/critical_engine.rs` against
//! `BtrfsEngine` over the injected [`crate::btrfs::SubvolumeDriver`] — never mocked — and by
//! `tests/critical_btrfs_live.rs` against the real kernel ioctl path.
//!
//! # Read-only snapshot vs writable clone
//!
//! [`WorldEngine`] keeps them as two distinct operations with distinct signatures:
//! [`WorldEngine::create_snapshot`] and [`WorldEngine::freeze_world`] produce an immutable
//! read-only snapshot, while [`WorldEngine::clone_world`] materializes a writable world from one.
//! There is no mode flag and no capability boolean to branch on (`ADR-0010` §3), so this suite
//! never passes one: it creates a read-only snapshot, clones a writable world from it, seals that
//! world once, and then proves the sealed result is still frozen.
//!
//! This shared matrix covers the `&dyn WorldEngine` behaviour only; driver-level teardown
//! invariants (clearing `BTRFS_SUBVOL_RDONLY` before `destroy_subvolume`) are asserted by the
//! driver-injected suite in `tests/critical_engine.rs`, which inspects the injected driver's
//! journal.
//!
//! [`assert_world_engine_conformance`] is `pub` on purpose: out-of-crate suites
//! (`crates/slipfork-engine/tests/critical_engine.rs`, and any future adapter suite) call it with
//! the adapter under test plus a scratch directory the adapter may write.

#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use slipfork_world::WorldTarget;

use crate::env::InheritedVar;
use crate::rootfs::WorldRootfs;
use crate::{EngineError, ExecEnv, MountMode, SnapshotId, WorldEngine, WorldMount, WorldSpec};

const SECRET_KEY: &str = "SLIPFORK_TEST_SECRET";
const SECRET_VALUE: &str = "conformance-secret-value";

/// Verify the shared fail-closed [`WorldEngine`] contract across any implementation.
///
/// The engine is borrowed as `&dyn WorldEngine` so an out-of-crate suite can pass its adapter
/// directly. `scratch_dir` must be a writable directory on the engine's own CoW substrate
/// (`BtrfsEngine` seeds its content-addressed rootfs there); `rootfs` is the contract-valid
/// [`WorldRootfs`] the adapter materializes worlds from.
///
/// Read-only snapshots and writable clones stay separate calls throughout: the suite never selects
/// a snapshot mode, it creates one read-only snapshot and clones writable worlds from it.
///
/// # Panics
///
/// Panics with a precise diagnostic when `engine` violates the contract.
pub fn assert_world_engine_conformance(
    engine: &dyn WorldEngine,
    scratch_dir: &Path,
    rootfs: &WorldRootfs,
) {
    assert!(!engine.name().is_empty(), "engine name must not be empty");

    // 1. Secrets never appear in Debug, including through a containing WorldSpec.
    let mut base_env = ExecEnv::world_scoped(scratch_dir.join("conformance-home"));
    base_env.inherit(InheritedVar::Path, "/usr/local/bin:/usr/bin:/bin");
    base_env
        .set_secret(SECRET_KEY, SECRET_VALUE)
        .expect("valid env name");
    let redacted = format!("{base_env:?}");
    assert!(
        !redacted.contains(SECRET_VALUE),
        "{} leaked a secret value through ExecEnv Debug",
        engine.name()
    );

    let missing = WorldSpec {
        target: WorldTarget::Canonical,
        rootfs: rootfs.clone(),
        snapshot: SnapshotId("definitely-missing-snapshot".to_owned()),
        mounts: Vec::new(),
        base_env: base_env.clone(),
        local_ingress: None,
    };
    let spec_debug = format!("{missing:?}");
    assert!(
        !spec_debug.contains(SECRET_VALUE),
        "{} leaked a secret value through WorldSpec Debug",
        engine.name()
    );

    // 2. Unknown snapshots fail closed instead of degrading to an empty world.
    let err = engine
        .clone_world(&missing)
        .expect_err("cloning an unknown snapshot must fail closed");
    assert!(
        matches!(err, EngineError::SnapshotNotFound(_)),
        "expected SnapshotNotFound, got {err:?}"
    );

    // 3. Nested read-write mounts are refused before any world state is materialized.
    let nested = WorldSpec {
        snapshot: SnapshotId("conformance-mounted".to_owned()),
        mounts: vec![
            WorldMount {
                host_path: scratch_dir.join("repo"),
                guest_path: scratch_dir.join("repo"),
                mode: MountMode::ReadWrite,
            },
            WorldMount {
                host_path: scratch_dir.join("repo").join("artifacts"),
                guest_path: scratch_dir.join("repo").join("artifacts"),
                mode: MountMode::ReadWrite,
            },
        ],
        ..missing.clone()
    };
    let err = engine
        .clone_world(&nested)
        .expect_err("nested read-write mounts must fail closed");
    assert!(
        matches!(err, EngineError::OverlappingMounts { .. }),
        "expected OverlappingMounts, got {err:?}"
    );

    // 4. Clone → opaque handle → seal-once freeze → destroy.
    let source = scratch_dir.join("conformance-source");
    fs::create_dir_all(&source).expect("create conformance source");
    fs::write(source.join("seed.txt"), b"conformance\n").expect("write conformance seed");
    let snapshot = SnapshotId("conformance-base".to_owned());
    engine
        .create_snapshot(&source, &snapshot)
        .expect("a read-only snapshot must succeed on the conformance scratch dir");

    let spec = WorldSpec {
        snapshot,
        ..missing
    };
    let world = engine.clone_world(&spec).expect("clone conformance world");
    let opaque = world.clone();
    assert_eq!(
        format!("{world:?}"),
        format!("{opaque:?}"),
        "{} handle Debug must be stable across clones",
        engine.name()
    );
    assert!(
        !format!("{world:?}").contains(SECRET_VALUE),
        "{} handle Debug must never expose world environment secrets",
        engine.name()
    );

    // 5. Entry commands name a real program, carry an argv prefix, and preserve the requested cwd.
    let entry_cwd = scratch_dir.join("repo");
    let entry = engine
        .entry_command(&world, &entry_cwd)
        .expect("every engine must supply an entry command for a live world");
    assert!(
        !entry.program.as_os_str().is_empty(),
        "{} entry program must not be empty",
        engine.name()
    );
    assert!(
        !entry.args_prefix.is_empty(),
        "{} entry command needs an argv prefix",
        engine.name()
    );
    assert_eq!(
        entry.cwd,
        entry_cwd,
        "{} entry command must preserve the requested cwd",
        engine.name()
    );

    // 6. Dialing a live world either works, or fails closed with a typed error — never a
    //    fabricated stream and never a hang.
    match engine.dial_world_tcp(&world, 1) {
        Ok(_) => panic!(
            "{} must not report a live stream for closed port 1",
            engine.name()
        ),
        Err(EngineError::Unsupported {
            engine: reported, ..
        }) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
        }
        Err(EngineError::DialFailed { .. }) => {}
        Err(other) => panic!(
            "{} must fail dial_world_tcp with a typed error, got {other:?}",
            engine.name()
        ),
    }

    // 7. Unknown services are typed; logs are never fabricated.
    match engine.service_logs(&world, "conformance-missing-service") {
        Ok(_) => panic!("{} must not fabricate service logs", engine.name()),
        Err(EngineError::Unsupported {
            engine: reported, ..
        }) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
        }
        Err(EngineError::UnknownService { .. }) => {}
        Err(other) => panic!(
            "{} must fail service_logs with a typed error, got {other:?}",
            engine.name()
        ),
    }

    engine
        .freeze_world(&world, &SnapshotId("conformance-frozen".to_owned()))
        .expect("a live world must be freezable");
    let err = engine
        .freeze_world(&world, &SnapshotId("conformance-frozen".to_owned()))
        .expect_err("an already-frozen world must fail closed");
    assert!(
        matches!(err, EngineError::WorldNotFreezable { .. }),
        "expected WorldNotFreezable, got {err:?}"
    );

    engine
        .destroy_world(world)
        .expect("destroy conformance world");
    let err = engine
        .freeze_world(&opaque, &SnapshotId("conformance-frozen".to_owned()))
        .expect_err("a destroyed world must fail closed");
    assert!(
        matches!(err, EngineError::WorldGone { .. }),
        "expected WorldGone for a destroyed world, got {err:?}"
    );
    let err = engine
        .destroy_world(opaque)
        .expect_err("destroying a world twice must fail closed");
    assert!(
        matches!(err, EngineError::WorldGone { .. }),
        "expected WorldGone for a double destroy, got {err:?}"
    );
}
