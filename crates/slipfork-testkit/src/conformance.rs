//! Shared [`WorldEngine`] conformance checks executed against every world engine adapter.
//!
//! The suite asserts only what every engine must satisfy *without* a live world: honest
//! fail-closed error types, mount-spec validation, environment redaction, opaque handle stability,
//! and seal-once freeze semantics. Real CoW lineage (frozen snapshot → independent sibling
//! clones, `$UID` ownership, real bind mounts) is asserted by `tests/critical_engine.rs` against
//! `slipfork-engine`'s `BtrfsEngine` over its injected `KernelBtrfsDriver` seam — never mocked — and by
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

use slipfork_world::InheritedVar;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, ExecEnv, MountMode, SnapshotId, WorldEngine, WorldMount, WorldSpec,
};

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
        boot: slipfork_world::BootLineage::default(),
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

    // 5. Entry commands name a real program, carry an argv prefix, and start the target on a
    //    directory that exists inside the world — never on the caller's host pathname, which has
    //    no reachable `getcwd(2)` after the world's `pivot_root` (the shell prints a `getcwd`
    //    failure). The conformance world mounts nothing, so the app stage root `/` is the honest
    //    start.
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
        Path::new("/"),
        "{} entry command must spawn in `/`, the one directory that exists on the host and inside \
         every world rootfs",
        engine.name()
    );
    // The world shares the host mount table 1:1, so the requested directory is the directory the
    // entry process must land on — after `setns`, and only via `--wdns` (never a pre-setns `--wd`).
    assert!(
        entry
            .args_prefix
            .iter()
            .any(|arg| *arg == format!("--wdns={}", entry_cwd.display())),
        "{} entry command must start in the requested directory ({}): {:?}",
        engine.name(),
        entry_cwd.display(),
        entry.args_prefix
    );
    assert!(
        !entry.args_prefix.iter().any(|arg| arg.starts_with("--wd=")),
        "{} entry command must open its directory after the namespace switch: {:?}",
        engine.name(),
        entry.args_prefix
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

    // 7. `reload_world` is honest and load-bearing: an adapter that can re-root a live world
    //    returns `Ok` after clearing supervised service rows and recording the reload spec's
    //    rootfs/lineage facts, while one that declines reports the typed, engine-named
    //    `Unsupported`. The world keeps its opaque handle across the reload.
    match engine.spawn_service(
        &world,
        &slipfork_world::ServiceSpec {
            id: "conformance-pre-reload".to_owned(),
            kind: slipfork_world::ServiceKind::Component,
            runtime: slipfork_world::ServiceRuntime::WorldProcess {
                argv: vec!["sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
                cwd: std::path::PathBuf::from("/"),
            },
            env: ExecEnv::default(),
            readiness_port: None,
        },
    ) {
        Ok(_handle) => {
            assert_eq!(
                engine
                    .world_processes(&world)
                    .expect("live world rows before reload")
                    .len(),
                1,
                "{} must record a spawned service before reload",
                engine.name()
            );
        }
        Err(EngineError::Unsupported {
            engine: reported, ..
        }) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
        }
        Err(other) => panic!(
            "{} must spawn a trivial world process or decline with Unsupported, got {other:?}",
            engine.name()
        ),
    }
    // The shared-init namespace contract: a live world answers with the three absolute joins a
    // service pod must carry, and an engine with no shared init reports the typed, engine-named
    // `Unsupported` instead of fabricating handles. This runs on the already-booted world above —
    // no second boot, no substitute runtime.
    match engine.world_pod_namespaces(&world) {
        Ok(namespaces) => {
            for (kind, path) in namespaces.entries() {
                assert!(
                    path.is_absolute(),
                    "{} {kind} namespace join '{}' must be absolute",
                    engine.name(),
                    path.display()
                );
                // `/proc/<pid>/ns/<kind>` is a procfs *magic* symlink: `Path::exists()` follows the
                // target (`user:[4026531837]`), which is not a filesystem path, so it always
                // reports `false`. Liveness is proven by the link itself instead: `lstat` must
                // succeed, and where the caller may read magic links the target must name the
                // namespace and its inode.
                let link = fs::symlink_metadata(path).unwrap_or_else(|err| {
                    panic!(
                        "{} {kind} namespace join '{}' must name a live namespace link: {err}",
                        engine.name(),
                        path.display()
                    )
                });
                assert!(
                    link.file_type().is_symlink(),
                    "{} {kind} namespace join '{}' must be a namespace symlink",
                    engine.name(),
                    path.display()
                );
                if let Ok(target) = fs::read_link(path) {
                    let target = target.display().to_string();
                    assert!(
                        target
                            .split_once(':')
                            .is_some_and(|(ns, inode)| !ns.is_empty()
                                && inode.starts_with('[')
                                && inode.ends_with(']')),
                        "{} {kind} namespace join '{}' must resolve to '<ns>:[<inode>]', got '{target}'",
                        engine.name(),
                        path.display()
                    );
                }
            }
        }
        Err(EngineError::Unsupported {
            engine: reported,
            capability,
        }) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
            assert_eq!(
                capability, "world_pod_namespaces",
                "the declined capability must be named"
            );
        }
        Err(other) => panic!(
            "{} must publish its shared-init namespaces or decline with Unsupported, got {other:?}",
            engine.name()
        ),
    }

    // A pod the engine cannot place must leave no usable row: either it is rejected before any
    // record exists, or it keeps the documented retained `Stopped` row with a closed log and no
    // live facts. Nothing may remain `Running`.
    let bogus_pod = slipfork_world::ServicePodSpec::new(
        slipfork_world::PodLocation::new(
            &scratch_dir.join("conformance-world"),
            &scratch_dir.join("conformance-foreign/app"),
            &scratch_dir.join("conformance-rootfs"),
        )
        .expect("absolute pod locations"),
        slipfork_world::PodNamespaces::new(
            &scratch_dir.join("conformance-ns/user"),
            &scratch_dir.join("conformance-ns/net"),
            &scratch_dir.join("conformance-ns/ipc"),
        )
        .expect("absolute namespace joins"),
        slipfork_world::VolumePlan::new(
            &scratch_dir.join("conformance-world"),
            "conformance-foreign",
            &scratch_dir.join("repo/conformance-foreign"),
            &[],
        )
        .expect("valid volume plan"),
        slipfork_world::PodCommand::new(
            scratch_dir.join("repo/conformance-foreign"),
            vec!["sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
            std::collections::BTreeMap::new(),
        ),
    )
    .expect("valid pod spec");
    match engine.spawn_service(
        &world,
        &slipfork_world::ServiceSpec {
            id: "conformance-foreign".to_owned(),
            kind: slipfork_world::ServiceKind::Component,
            runtime: slipfork_world::ServiceRuntime::ServicePod(bogus_pod),
            env: ExecEnv::default(),
            readiness_port: None,
        },
    ) {
        Ok(_) => panic!(
            "{} must refuse a pod it cannot place in the world",
            engine.name()
        ),
        Err(EngineError::InvalidWorldSpec { .. }) | Err(EngineError::Unsupported { .. }) => {}
        Err(other) => panic!(
            "{} must refuse an unplaceable pod with a typed error, got {other:?}",
            engine.name()
        ),
    }
    if let Some(row) = engine
        .world_processes(&world)
        .expect("a live world must report its row set")
        .into_iter()
        .find(|row| row.id == "conformance-foreign")
    {
        assert_eq!(
            row.state,
            slipfork_world::ServiceState::Stopped,
            "{} kept a rejected pod runnable: {row:?}",
            engine.name()
        );
        assert!(
            row.pid.is_none() && row.port.is_none(),
            "{} reported live facts for a rejected pod: {row:?}",
            engine.name()
        );
        assert!(
            engine.service_logs(&world, "conformance-foreign").is_ok(),
            "{} dropped the retained log of a rejected pod",
            engine.name()
        );
    }

    let (reloaded_cache, reloaded_millis) = match rootfs.cache {
        slipfork_world::CacheOutcome::Hit => (slipfork_world::CacheOutcome::Miss, Some(42)),
        slipfork_world::CacheOutcome::Miss => (slipfork_world::CacheOutcome::Hit, None),
    };
    let reloaded_spec = WorldSpec {
        rootfs: WorldRootfs {
            cache: reloaded_cache,
            millis: reloaded_millis,
            ..rootfs.clone()
        },
        boot: slipfork_world::BootLineage {
            base: Some(slipfork_world::BaseLineage {
                key: "conformance-reloaded-base".to_owned(),
                millis: Some(7),
            }),
            seed: None,
        },
        ..spec.clone()
    };
    let reload_supported = match engine.reload_world(&world, &reloaded_spec) {
        Ok(()) => {
            let cleared = engine
                .world_processes(&world)
                .expect("live world must report its row set after reload");
            assert!(
                cleared.is_empty(),
                "{} reload_world returned Ok without clearing service rows: {cleared:?}",
                engine.name()
            );
            let reloaded_lineage = engine
                .world_lineage(&world)
                .expect("live world must report its lineage after reload");
            assert_eq!(
                reloaded_lineage.rootfs,
                Some(slipfork_world::RootfsLineage {
                    digest: reloaded_spec.rootfs.digest.hex().to_owned(),
                    cache: reloaded_cache,
                    millis: reloaded_millis,
                }),
                "{} reload_world returned Ok without recording spec.rootfs lineage",
                engine.name()
            );
            assert_eq!(
                reloaded_lineage.base,
                reloaded_spec.boot.base,
                "{} reload_world returned Ok without recording spec.boot.base",
                engine.name()
            );
            true
        }
        Err(EngineError::Unsupported {
            engine: reported,
            capability,
        }) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
            assert_eq!(
                capability, "reload_world",
                "the declined capability must be named"
            );
            false
        }
        Err(other) => panic!(
            "{} must reload a live world or decline with a typed Unsupported, got {other:?}",
            engine.name()
        ),
    };
    assert_eq!(
        format!("{world:?}"),
        format!("{opaque:?}"),
        "{} handle Debug must be stable across a reload",
        engine.name()
    );

    // 8. The engine.s own process and lineage records are honest: unique ids, no live fact on a
    //    non-running row, and a cache `hit` never claims a measured duration.
    let rows = engine
        .world_processes(&world)
        .expect("a live world must report its row set");
    let mut ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    ids.sort_unstable();
    let unique = ids.len();
    ids.dedup();
    assert_eq!(
        unique,
        ids.len(),
        "{} reported a duplicate service id",
        engine.name()
    );
    for row in &rows {
        if row.state != slipfork_world::ServiceState::Running {
            assert!(
                row.pid.is_none() && row.port.is_none() && row.uptime_secs.is_none(),
                "{} reported live facts on the non-running row '{}': {row:?}",
                engine.name(),
                row.id
            );
        }
    }
    assert!(
        rows.is_empty(),
        "{} fabricated rows for a world with no spawned service",
        engine.name()
    );
    let lineage = engine
        .world_lineage(&world)
        .expect("a live world must report its lineage");
    assert!(
        engine
            .world_is_live(&world)
            .expect("a live world must answer its own liveness"),
        "{} must report its booted world as live",
        engine.name()
    );
    if let Some(lineage_rootfs) = &lineage.rootfs {
        assert_eq!(
            lineage_rootfs.digest,
            rootfs.digest.hex(),
            "{} lineage digest must be the materialized rootfs Merkle key",
            engine.name()
        );
        assert_lineage_cache_is_honest(
            engine,
            "rootfs",
            lineage_rootfs.cache,
            lineage_rootfs.millis,
        );
    }
    for service in &lineage.services {
        assert_lineage_cache_is_honest(engine, &service.id, service.cache, service.millis);
    }

    // 9. Unknown services are typed; logs are never fabricated.
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

    // A live world's services can be stopped as one ordered set; the call is idempotent.
    engine
        .stop_world_services(&world)
        .expect("stopping a live world's services is idempotent");

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
    // Sealing is a liveness transition: the three answers are pinned together — a booted world is
    // live, a frozen one is not live (never an error, never `true`), and a destroyed handle fails
    // closed with `WorldGone` below. An engine that answers `status == Live` alone, or that derives
    // "live" from holding the handle, fails here.
    assert!(
        !engine
            .world_is_live(&world)
            .expect("a frozen world must answer its liveness"),
        "{} must report a frozen world as not live",
        engine.name()
    );
    // A frozen world is immutable: an engine implementing `reload_world` must refuse to re-root it
    // with `WorldNotReloadable`; one that declines the capability reports the same `Unsupported`.
    match (reload_supported, engine.reload_world(&world, &spec)) {
        (true, Err(EngineError::WorldNotReloadable { .. })) => {}
        (
            false,
            Err(EngineError::Unsupported {
                engine: reported,
                capability,
            }),
        ) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
            assert_eq!(
                capability, "reload_world",
                "the declined capability must be named"
            );
        }
        (_, other) => panic!(
            "{} reload_world on a frozen world must fail closed with {} (reload_supported={reload_supported}), got {other:?}",
            engine.name(),
            if reload_supported {
                "WorldNotReloadable"
            } else {
                "Unsupported"
            }
        ),
    }

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
    for (method, err) in [
        (
            "world_processes",
            engine.world_processes(&opaque).map(|_| ()),
        ),
        ("world_lineage", engine.world_lineage(&opaque).map(|_| ())),
        ("world_is_live", engine.world_is_live(&opaque).map(|_| ())),
        ("stop_world_services", engine.stop_world_services(&opaque)),
    ] {
        match err {
            Err(EngineError::WorldGone { .. }) => {}
            other => panic!(
                "{} {method} on a destroyed world must fail closed with WorldGone, got {other:?}",
                engine.name()
            ),
        }
    }
    // On a destroyed handle, an engine whose live reload succeeded MUST fail closed with
    // `WorldGone` (never `Unsupported`); `Unsupported` is permitted only when the live call itself
    // declined the capability.
    match (reload_supported, engine.reload_world(&opaque, &spec)) {
        (true, Err(EngineError::WorldGone { .. })) => {}
        (
            false,
            Err(EngineError::Unsupported {
                engine: reported,
                capability,
            }),
        ) => {
            assert_eq!(reported, engine.name(), "Unsupported must name the engine");
            assert_eq!(
                capability, "reload_world",
                "the declined capability must be named"
            );
        }
        (_, other) => panic!(
            "{} reload_world on a destroyed world must fail closed with {} (reload_supported={reload_supported}), got {other:?}",
            engine.name(),
            if reload_supported {
                "WorldGone"
            } else {
                "Unsupported"
            }
        ),
    }
    let err = engine
        .destroy_world(opaque)
        .expect_err("destroying a world twice must fail closed");
    assert!(
        matches!(err, EngineError::WorldGone { .. }),
        "expected WorldGone for a double destroy, got {err:?}"
    );
}

/// A recorded cache outcome must match its duration: a `hit` ran no work, so it reports `None`
/// (never `0`), while a `miss` may report a measured `Some(millis)`.
fn assert_lineage_cache_is_honest(
    engine: &dyn WorldEngine,
    node: &str,
    cache: slipfork_world::CacheOutcome,
    millis: Option<u64>,
) {
    if cache == slipfork_world::CacheOutcome::Hit {
        assert_eq!(
            millis,
            None,
            "{} reported `hit` with a duration for {node}; a cache hit measured nothing",
            engine.name()
        );
    }
}
