//! Shared [`WorldEngine`] conformance checks executed against every hypervisor/CoW driver.
//!
//! The suite asserts only what every engine must satisfy *without* a live hypervisor: honest
//! fail-closed error types, mount-spec validation, environment redaction, opaque handle stability,
//! and seal-once freeze semantics. Real CoW lineage (frozen snapshot → independent sibling
//! clones, `$UID` ownership, real bind mounts) is asserted by `tests/critical_engine.rs` against
//! `ReflinkNetnsEngine`'s real `ioctl(FICLONE)` + namespace path — never mocked.

#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use slipfork_world::WorldTarget;

use crate::env::InheritedVar;
use crate::{
    EngineError, ExecEnv, MountMode, SnapshotId, SnapshotMode, WorldEngine, WorldMount, WorldSpec,
};

const SECRET_KEY: &str = "SLIPFORK_TEST_SECRET";
const SECRET_VALUE: &str = "conformance-secret-value";

/// Verify the shared fail-closed [`WorldEngine`] contract across any implementation.
///
/// # Panics
///
/// Panics with a precise diagnostic when `engine` violates the contract.
pub fn assert_world_engine_conformance(engine: &dyn WorldEngine, scratch_dir: &Path) {
    assert!(!engine.name().is_empty(), "engine name must not be empty");

    // 1. Stateful RAM snapshots are either honestly supported or typed fail-closed. There is no
    //    capability boolean: the error variant itself is the contract.
    match engine.create_snapshot(
        scratch_dir,
        &SnapshotId("conformance-ram".to_owned()),
        SnapshotMode::StatefulRam,
    ) {
        Ok(()) => {}
        Err(EngineError::StatefulRamUnsupported { engine: reported }) => {
            assert_eq!(
                reported,
                engine.name(),
                "StatefulRamUnsupported must name the implementing engine"
            );
        }
        Err(other) => panic!(
            "{} must support StatefulRam or fail closed with StatefulRamUnsupported, got {other:?}",
            engine.name()
        ),
    }

    // 2. Secrets never appear in Debug, including through a containing WorldSpec.
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
        snapshot: SnapshotId("definitely-missing-snapshot".to_owned()),
        mounts: Vec::new(),
        base_env: base_env.clone(),
        local_ingress_port: None,
    };
    let spec_debug = format!("{missing:?}");
    assert!(
        !spec_debug.contains(SECRET_VALUE),
        "{} leaked a secret value through WorldSpec Debug",
        engine.name()
    );

    // 3. Unknown snapshots fail closed instead of degrading to an empty world.
    let err = engine
        .clone_world(&missing)
        .expect_err("cloning an unknown snapshot must fail closed");
    assert!(
        matches!(err, EngineError::SnapshotNotFound(_)),
        "expected SnapshotNotFound, got {err:?}"
    );

    // 4. Nested read-write mounts are refused before any world state is materialized.
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

    // 5. Clone → opaque handle → seal-once freeze → destroy.
    let source = scratch_dir.join("conformance-source");
    fs::create_dir_all(&source).expect("create conformance source");
    fs::write(source.join("seed.txt"), b"conformance\n").expect("write conformance seed");
    let snapshot = SnapshotId("conformance-base".to_owned());
    engine
        .create_snapshot(&source, &snapshot, SnapshotMode::DiskCow)
        .expect("disk CoW snapshot must succeed on the conformance scratch dir");

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

    engine
        .freeze_world(
            &world,
            &SnapshotId("conformance-frozen".to_owned()),
            SnapshotMode::DiskCow,
        )
        .expect("a live world must be freezable");
    let err = engine
        .freeze_world(
            &world,
            &SnapshotId("conformance-frozen".to_owned()),
            SnapshotMode::DiskCow,
        )
        .expect_err("an already-frozen world must fail closed");
    assert!(
        matches!(err, EngineError::WorldNotFreezable { .. }),
        "expected WorldNotFreezable, got {err:?}"
    );

    engine
        .destroy_world(world)
        .expect("destroy conformance world");
    let err = engine
        .freeze_world(
            &opaque,
            &SnapshotId("conformance-frozen".to_owned()),
            SnapshotMode::DiskCow,
        )
        .expect_err("a destroyed world must fail closed");
    assert!(
        matches!(err, EngineError::WorldNotFreezable { .. }),
        "expected WorldNotFreezable for a destroyed world, got {err:?}"
    );
    let err = engine
        .destroy_world(opaque)
        .expect_err("destroying a world twice must fail closed");
    assert!(
        matches!(err, EngineError::WorldGone { .. }),
        "expected WorldGone for a double destroy, got {err:?}"
    );
}
