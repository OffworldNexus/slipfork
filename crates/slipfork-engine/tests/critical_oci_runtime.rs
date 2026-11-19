//! Protected kernel-independent contract tests for daemonless `crun` backing services
//! (`ADR-0009` §2–§3).
//!
//! Everything here runs on any host: the suite boots a real world over the shared
//! the real [`slipfork_engine::btrfs::KernelBtrfsDriver`] and asserts every container contract the engine
//! refuses **before** it execs `crun` — request shape, backing-image presence in `WorldRootfs`,
//! state-subdirectory lineage, read-only mount visibility and collisions, and the typed
//! `ServiceNotRunning`/`UnknownService`/`WorldGone` surfaces.
//!
//! The live `postgres:18` + `redis:8` runtime proof (same in-world port in two Btrfs clones, real
//! `psql`/`redis-cli` queries, `--ipc` isolation, dump/restore through a read-only mount) is
//! `critical_btrfs_live.rs`: it needs real `BTRFS_IOC_SNAP_CREATE_V2` clones, so it runs on the
//! Btrfs host. No assertion here claims runtime behaviour.

pub mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::WorldTarget;
use slipfork_world::rootfs::BackingServiceKey;
use slipfork_world::rootfs::OciRootfs;
use slipfork_world::{
    BootLineage, CacheOutcome, EngineError, ExecEnv, MountMode, ServiceKind, ServiceRuntime,
    ServiceSpec, StateSubdirMount, WorldEngine, WorldMount, WorldSpec,
};
use support::{PG_DATA, WorldFixture, pg_state};

/// The image's own `PATH` allowlist; this suite never executes a container.
const SERVICE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A backing-service rootfs carrying only the mount destinations the bundle validator reads.
///
/// This is the engine's *validation* input, not a runtime proof: the digest-pinned real images
/// (`postgres:18`, `redis:8`) are materialized and run by `critical_btrfs_live.rs`.
fn stub_image(root: &Path, name: &str, digest_byte: char, entries: &[&str]) -> OciRootfs {
    let tree = root.join("oci").join(name);
    for entry in entries {
        let target = tree.join(entry.trim_start_matches('/'));
        if entry.ends_with('/') {
            fs::create_dir_all(&target).expect("stub image directory");
        } else {
            fs::create_dir_all(target.parent().expect("entry parent")).expect("stub image parent");
            fs::write(&target, b"").expect("stub image file");
        }
    }
    let config = tree.join("config.json");
    fs::write(&config, b"{}").expect("stub image config");
    OciRootfs {
        digest: format!("sha256:{}", digest_byte.to_string().repeat(64)),
        rootfs: tree,
        config,
        cache: CacheOutcome::Hit,
        millis: None,
    }
}

fn postgres_image() -> BackingServiceKey {
    BackingServiceKey::Postgres {
        version: "18".to_owned(),
    }
}

fn redis_image() -> BackingServiceKey {
    BackingServiceKey::Redis {
        version: "8".to_owned(),
    }
}

/// One fixture carrying the two stub backing-service images the pre-spawn validator reads.
fn oci_fixture(label: &str) -> WorldFixture {
    let mut fixture = WorldFixture::new(label);
    fixture.rootfs.service_images = BTreeMap::from([
        (
            postgres_image(),
            stub_image(
                &fixture.root,
                "postgres-18",
                '0',
                &["/var/lib/postgresql/data/", "/dumps/seed.pgdump"],
            ),
        ),
        (
            redis_image(),
            stub_image(&fixture.root, "redis-8", '1', &["/data/"]),
        ),
    ]);
    fixture
}

fn oci_service(
    id: &str,
    image: BackingServiceKey,
    state_mounts: Vec<StateSubdirMount>,
    readonly_mounts: Vec<WorldMount>,
) -> ServiceSpec {
    ServiceSpec {
        id: id.to_owned(),
        kind: ServiceKind::BackingService,
        runtime: ServiceRuntime::OciContainer {
            image,
            argv: vec!["sleep".to_owned(), "infinity".to_owned()],
            state_mounts,
            readonly_mounts,
        },
        env: support::container_env(SERVICE_PATH),
        readiness_port: None,
    }
}

fn expect_invalid(service: &ServiceSpec, err: EngineError, needle: &str) {
    match err {
        EngineError::InvalidWorldSpec { reason } => assert!(
            reason.contains(needle),
            "service '{}' diagnostic must contain '{needle}': {reason}",
            service.id
        ),
        other => panic!(
            "service '{}': expected InvalidWorldSpec, got {other:?}",
            service.id
        ),
    }
}

/// Collect every synthesized OCI bundle `config.json` under the world control tree.
fn collect_bundle_configs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("read control directory entry").path();
        if path.is_dir() {
            collect_bundle_configs(&path, out);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("config.json") {
            out.push(path);
        }
    }
}

#[test]
fn container_requests_fail_closed_before_crun_is_executed() {
    let fixture = oci_fixture("oci-requests");
    let world = fixture.world_with_artifacts(WorldTarget::Canonical);

    // 1. Request shape is validated before the world is even consulted.
    let empty_id = oci_service("", postgres_image(), pg_state(), Vec::new());
    let err = fixture
        .engine
        .spawn_service(&world, &empty_id)
        .expect_err("an empty service id must fail closed");
    expect_invalid(&empty_id, err, "must not be empty");

    let mut empty_argv = oci_service("no-argv", postgres_image(), pg_state(), Vec::new());
    empty_argv.runtime = ServiceRuntime::OciContainer {
        image: postgres_image(),
        argv: Vec::new(),
        state_mounts: pg_state(),
        readonly_mounts: Vec::new(),
    };
    let err = fixture
        .engine
        .spawn_service(&world, &empty_argv)
        .expect_err("an empty container argv must fail closed");
    expect_invalid(&empty_argv, err, "argv must not be empty");

    // 2. The backing image must be materialized into `WorldRootfs` under its `Forkfile` key.
    let mut missing_rootfs = fixture.rootfs.clone();
    missing_rootfs.service_images.clear();
    let world_without_images = fixture
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::UserFork("no-images".to_owned()),
            rootfs: missing_rootfs,
            snapshot: fixture.snapshot.clone(),
            mounts: Vec::new(),
            base_env: support::world_env(&fixture.home),
            local_ingress: None,
            boot: BootLineage::default(),
        })
        .expect("clone world without backing images");
    let missing = oci_service("pg", postgres_image(), pg_state(), Vec::new());
    let err = fixture
        .engine
        .spawn_service(&world_without_images, &missing)
        .expect_err("an unmaterialized backing image must fail closed");
    expect_invalid(&missing, err, "carries no materialized OCI rootfs");

    // 3. A backing image whose materialized rootfs disappears is refused, never handed to `crun`.
    let mut removed_rootfs = fixture.rootfs.clone();
    let removed_image = stub_image(
        &fixture.root,
        "postgres-18-removed",
        '2',
        &["/var/lib/postgresql/data/"],
    );
    let pg_rootfs = removed_image.rootfs.clone();
    removed_rootfs
        .service_images
        .insert(postgres_image(), removed_image);
    let world_with_removed_image = fixture
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::UserFork("removed-image".to_owned()),
            rootfs: removed_rootfs,
            snapshot: fixture.snapshot.clone(),
            mounts: Vec::new(),
            base_env: support::world_env(&fixture.home),
            local_ingress: None,
            boot: BootLineage::default(),
        })
        .expect("clone world with a materialized image rootfs");
    fs::remove_dir_all(&pg_rootfs).expect("remove the materialized image rootfs");
    let removed_service = oci_service("pg", postgres_image(), pg_state(), Vec::new());
    let err = fixture
        .engine
        .spawn_service(&world_with_removed_image, &removed_service)
        .expect_err("an absent backing image rootfs must fail closed");
    expect_invalid(&removed_service, err, "does not exist");

    // 4. State subdirectories stay plain paths under the world state dir.
    let mut absolute_state = oci_service("pg", postgres_image(), Vec::new(), Vec::new());
    absolute_state.runtime = ServiceRuntime::OciContainer {
        image: postgres_image(),
        argv: vec!["sleep".to_owned(), "infinity".to_owned()],
        state_mounts: vec![StateSubdirMount {
            host_subdir: PathBuf::from("/etc"),
            container_path: PathBuf::from(PG_DATA),
        }],
        readonly_mounts: Vec::new(),
    };
    let err = fixture
        .engine
        .spawn_service(&world, &absolute_state)
        .expect_err("an absolute state subdirectory must fail closed");
    expect_invalid(&absolute_state, err, "must be relative");

    let mut escaping_state = oci_service("pg", postgres_image(), Vec::new(), Vec::new());
    escaping_state.runtime = ServiceRuntime::OciContainer {
        image: postgres_image(),
        argv: vec!["sleep".to_owned(), "infinity".to_owned()],
        state_mounts: vec![StateSubdirMount {
            host_subdir: PathBuf::from("../escape"),
            container_path: PathBuf::from(PG_DATA),
        }],
        readonly_mounts: Vec::new(),
    };
    let err = fixture
        .engine
        .spawn_service(&world, &escaping_state)
        .expect_err("an escaping state subdirectory must fail closed");
    expect_invalid(&escaping_state, err, "plain relative path");

    let mut escaping_container = oci_service("pg", postgres_image(), Vec::new(), Vec::new());
    escaping_container.runtime = ServiceRuntime::OciContainer {
        image: postgres_image(),
        argv: vec!["sleep".to_owned(), "infinity".to_owned()],
        state_mounts: vec![StateSubdirMount {
            host_subdir: PathBuf::from("pgdata/main"),
            container_path: PathBuf::from("/data/../../etc"),
        }],
        readonly_mounts: Vec::new(),
    };
    let err = fixture
        .engine
        .spawn_service(&world, &escaping_container)
        .expect_err("an escaping container path must fail closed");
    expect_invalid(&escaping_container, err, "must not contain '..'");

    // 5. A destination the image does not carry is refused before `write_bundle` runs.
    let mut absent_destination = oci_service("pg", postgres_image(), Vec::new(), Vec::new());
    absent_destination.runtime = ServiceRuntime::OciContainer {
        image: postgres_image(),
        argv: vec!["sleep".to_owned(), "infinity".to_owned()],
        state_mounts: vec![StateSubdirMount {
            host_subdir: PathBuf::from("pgdata/main"),
            container_path: PathBuf::from("/not/in/the/image"),
        }],
        readonly_mounts: Vec::new(),
    };
    let err = fixture
        .engine
        .spawn_service(&world, &absent_destination)
        .expect_err("a missing image mount destination must fail closed");
    expect_invalid(
        &absent_destination,
        err,
        "does not exist in backing-service image",
    );

    // A refused spawn keeps its retained log for diagnosis and never reports `Running`.
    let logs = fixture
        .engine
        .service_logs(&world, "pg")
        .expect("refused spawn keeps its log");
    assert!(
        logs.is_closed(),
        "a refused spawn must close its captured log"
    );

    fixture
        .engine
        .destroy_world(world_with_removed_image)
        .expect("destroy removed-image world");
    fixture
        .engine
        .destroy_world(world_without_images)
        .expect("destroy no-images world");
    fixture.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&fixture.root);
}

#[test]
fn readonly_mounts_are_typed_visible_and_collision_free() {
    let fixture = oci_fixture("oci-readonly");
    let world = fixture.world_with_artifacts(WorldTarget::Canonical);

    // 1. A readonly mount must carry `MountMode::ReadOnly`.
    let wrong_mode = oci_service(
        "pg-wrong-mode",
        postgres_image(),
        Vec::new(),
        vec![WorldMount {
            host_path: fixture.artifacts.clone(),
            guest_path: PathBuf::from("/dumps/seed.pgdump"),
            mode: MountMode::ReadWrite,
        }],
    );
    let err = fixture
        .engine
        .spawn_service(&world, &wrong_mode)
        .expect_err("a ReadWrite mode on a readonly mount must fail closed");
    expect_invalid(&wrong_mode, err, "must carry MountMode::ReadOnly");

    // 2. The host path must exist before it is bound, so the world shows it 1:1.
    let invisible = oci_service(
        "pg-invisible",
        postgres_image(),
        Vec::new(),
        vec![WorldMount {
            host_path: fixture.root.join("nowhere"),
            guest_path: PathBuf::from("/dumps/seed.pgdump"),
            mode: MountMode::ReadOnly,
        }],
    );
    let err = fixture
        .engine
        .spawn_service(&world, &invisible)
        .expect_err("an invisible readonly mount must fail closed");
    expect_invalid(&invisible, err, "does not exist; stage it");

    // 3. A readonly mount that shadows a state mount is refused before a bundle exists.
    let colliding = oci_service(
        "pg-collision",
        postgres_image(),
        pg_state(),
        vec![WorldMount {
            host_path: fixture.artifacts.clone(),
            guest_path: PathBuf::from(PG_DATA),
            mode: MountMode::ReadOnly,
        }],
    );
    let err = fixture
        .engine
        .spawn_service(&world, &colliding)
        .expect_err("a colliding readonly/state mount must fail closed");
    expect_invalid(&colliding, err, "collides");
    assert!(
        !fixture
            .state_dir
            .join("ctl")
            .join("canonical-1")
            .join("oci")
            .join("pg-collision")
            .join("config.json")
            .exists(),
        "a refused spec must never leave a synthesized `crun` bundle"
    );

    // 4. The same readonly mount at its image destination passes every pre-spawn validation; the
    //    runtime proof (the mount is `EROFS`, dump/restore succeeds) is `critical_btrfs_live.rs`.
    //    Nothing is spawned here: the stub image carries no runnable payload.

    fixture.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&fixture.root);
}

#[test]
fn service_handles_and_logs_are_typed_and_never_fabricated() {
    let fixture = oci_fixture("oci-handles");
    let world = fixture.world_with_artifacts(WorldTarget::Canonical);

    // An app component is not a container: `exec_in_service` fails closed, typed.
    let app = fixture
        .engine
        .spawn_service(
            &world,
            &ServiceSpec {
                id: "app".to_owned(),
                kind: ServiceKind::Component,
                runtime: ServiceRuntime::WorldProcess {
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "exec sleep 300".to_owned(),
                    ],
                    cwd: fixture.repo.clone(),
                },
                env: ExecEnv::default(),
                readiness_port: None,
            },
        )
        .expect("spawn app component");
    // A world process is a container: `exec_in_service` and `service_entry_command` work, and an
    // unknown id still fails closed with a typed error (never a fabricated handle or log).
    let exec = fixture
        .engine
        .exec_in_service(&app, &["true".to_owned()], &ExecEnv::default(), &mut |_| {})
        .expect("exec inside the app container");
    assert_eq!(exec.exit_code, 0, "stderr: {}", exec.stderr);
    let entry = fixture
        .engine
        .service_entry_command(&app)
        .expect("the app container answers an entry command");
    assert!(
        entry.args_prefix.contains(&"exec".to_owned()),
        "{:?}",
        entry.args_prefix
    );

    let err = fixture
        .engine
        .service_logs(&world, "never-spawned")
        .expect_err("logs are never fabricated");
    assert!(matches!(err, EngineError::UnknownService { .. }));

    // After teardown every service surface fails closed on the world.
    fixture.engine.stop_service(&app).expect("stop app");
    fixture
        .engine
        .destroy_world(world.clone())
        .expect("destroy");
    let err = fixture
        .engine
        .spawn_service(
            &world,
            &oci_service("pg", postgres_image(), pg_state(), Vec::new()),
        )
        .expect_err("a destroyed world cannot host a service");
    assert!(
        matches!(err, EngineError::WorldGone { .. }),
        "expected WorldGone, got {err:?}"
    );
    let err = fixture
        .engine
        .service_logs(&world, "pg")
        .expect_err("a destroyed world has no services");
    assert!(matches!(err, EngineError::WorldGone { .. }));
    let _ = fs::remove_dir_all(&fixture.root);
}

/// The world component environment is persisted verbatim into an OCI bundle, so a backing
/// service must never receive it (resolved `modes.*.envs.*` secrets included); a world process
/// still must. This is the on-disk contract, asserted before the stub image can ever run.
#[test]
fn container_bundles_never_carry_the_world_environment() {
    const MARKER: &str = "slipfork-engine-world-env-marker";
    let fixture = oci_fixture("oci-env");
    let mut base_env = support::world_env(&fixture.home);
    base_env
        .set_secret("WORLD_SECRET_MARKER", MARKER)
        .expect("bind the marked world secret");
    base_env
        .set("WORLD_PLAIN_MARKER", MARKER)
        .expect("bind the marked world variable");
    let world = fixture
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: fixture.rootfs.clone(),
            snapshot: fixture.snapshot.clone(),
            mounts: vec![WorldMount {
                host_path: fixture.repo.clone(),
                guest_path: fixture.repo.clone(),
                mode: MountMode::ReadWrite,
            }],
            base_env,
            local_ingress: None,
            boot: BootLineage::default(),
        })
        .expect("clone world with a marked environment");

    // The stub image carries no runnable payload, but `write_bundle` runs before `crun` is exec'd:
    // that `config.json` is exactly the plaintext-on-disk surface this test guards.
    let container = fixture.engine.spawn_service(
        &world,
        &oci_service("pg", postgres_image(), pg_state(), Vec::new()),
    );

    let ctl = fixture.state_dir.join("ctl");
    let entry = fixture
        .engine
        .entry_command(&world, &fixture.repo)
        .expect("entry command for the world whose bundle tree is being read");
    let init_pid: u32 = entry
        .args_prefix
        .iter()
        .find_map(|arg| arg.strip_prefix("--target="))
        .and_then(|pid| pid.parse().ok())
        .expect("the entry command names the world init pid");
    // The bundles live in the world init's private tmpfs: read them through its mount namespace.
    let mounted_ctl = Path::new("/proc")
        .join(init_pid.to_string())
        .join("root")
        .join(ctl.strip_prefix("/").unwrap_or(&ctl));
    let mut bundles = Vec::new();
    collect_bundle_configs(&mounted_ctl, &mut bundles);
    assert!(
        !bundles.is_empty(),
        "the container spawn must have synthesized a bundle config.json under {}",
        mounted_ctl.display()
    );
    // Positive control: the bundle does carry the container's own declared environment, so the
    // absence assertions below prove filtering rather than an empty `process.env`.
    assert!(
        bundles.iter().any(|config| {
            let text = fs::read_to_string(config).unwrap_or_default();
            text.contains(&format!("PATH={SERVICE_PATH}")) && text.contains("\"LANG=C\"")
        }),
        "the bundle must carry the container's own env (`PATH`, `LANG`) under {}",
        mounted_ctl.display()
    );
    for config in &bundles {
        let text = fs::read_to_string(config).expect("read the bundle config");
        for needle in [MARKER, "WORLD_SECRET_MARKER", "WORLD_PLAIN_MARKER"] {
            assert!(
                !text.contains(needle),
                "container bundle {} leaked `{needle}` from the world component environment:\n{text}",
                config.display()
            );
        }
    }

    // A world process still receives the world env: it writes it into the shared repo mount.
    let dump = fixture.repo.join("world-env.dump");
    let app = fixture
        .engine
        .spawn_service(
            &world,
            &ServiceSpec {
                id: "app".to_owned(),
                kind: ServiceKind::Component,
                runtime: ServiceRuntime::WorldProcess {
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "env > world-env.dump && exec sleep 300".to_owned(),
                    ],
                    cwd: fixture.repo.clone(),
                },
                env: ExecEnv::default(),
                readiness_port: None,
            },
        )
        .expect("spawn the app component with the world environment");
    // The shell truncates the dump before `env` writes it, so wait for the completed document
    // rather than for the file's mere existence.
    slipfork_testkit::poll_until(Duration::from_secs(20), || {
        let text = fs::read_to_string(&dump).map_err(|err| format!("world env dump: {err}"))?;
        let complete = text.ends_with('\n')
            && ["WORLD_SECRET_MARKER=", "WORLD_PLAIN_MARKER="]
                .iter()
                .all(|needle| text.contains(needle));
        if complete {
            Ok(())
        } else {
            Err("world env dump is still being written".to_owned())
        }
    })
    .expect("the world process must write its environment");
    let observed = fs::read_to_string(&dump).expect("read the world env dump");
    for needle in [
        format!("WORLD_SECRET_MARKER={MARKER}"),
        format!("WORLD_PLAIN_MARKER={MARKER}"),
    ] {
        assert!(
            observed.contains(&needle),
            "a world process must still receive `{needle}`:\n{observed}"
        );
    }

    fixture.engine.stop_service(&app).expect("stop app");
    if let Ok(container) = container {
        fixture
            .engine
            .stop_service(&container)
            .expect("stop container");
    }
    fixture.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&fixture.root);
}
