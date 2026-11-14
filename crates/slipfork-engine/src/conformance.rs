//! Shared [`WorldEngine`] conformance checks executed against every hypervisor/CoW driver.

#![forbid(unsafe_code)]

use std::path::Path;

use crate::{EngineError, HostBindMount, SnapshotId, WorldEngine};

/// Verify core fail-closed [`WorldEngine`] invariants across any implementation:
/// non-empty identifier, core isolation flags (`disk_cow`, `isolated_netns`, `uid_passthrough`),
/// honest `stateful_ram_snapshot` refusal when unsupported, and `SnapshotNotFound` on unknown snapshots.
pub fn assert_world_engine_conformance(engine: &dyn WorldEngine, scratch_dir: &Path) {
    assert!(!engine.name().is_empty());
    let caps = engine.capabilities();
    assert!(
        caps.disk_cow,
        "{} must provide CoW disk cloning",
        engine.name()
    );
    assert!(
        caps.isolated_netns,
        "{} must isolate network ports",
        engine.name()
    );
    assert!(
        caps.uid_passthrough,
        "{} must preserve host UID/GID",
        engine.name()
    );

    if !caps.stateful_ram_snapshot {
        let err = engine
            .create_snapshot(scratch_dir, "ram-test", true)
            .expect_err("engine without stateful_ram_snapshot must fail closed");
        assert!(matches!(err, EngineError::StatefulRamUnsupported { .. }));
    }

    let missing_res = engine.clone_world(
        &SnapshotId("definitely-missing-snapshot".to_owned()),
        "w-missing",
        &HostBindMount {
            host_path: scratch_dir.to_path_buf(),
        },
    );
    assert!(
        matches!(missing_res, Err(EngineError::SnapshotNotFound(_))),
        "expected SnapshotNotFound, got {missing_res:?}"
    );
}
