//! Protected live kernel test: the real Btrfs ioctl + `crun` service path (`ADR-0010` §§2–3, §6).
//!
//! The suite probes `statfs(<state_dir>).f_type` exactly once and asserts the contract the host can
//! honour:
//!
//! - **Btrfs host** (the `M3.16`/`M3.19` DO VM): one real `BTRFS_IOC_SNAP_CREATE_V2` read-only
//!   snapshot, two real writable clones of it, `postgres:18` and `redis:8` `crun` services in both
//!   clones on the **same** in-world port, SysV-IPC isolation across the clones (the private
//!   `--ipc` namespace, not a mocked one), a freeze that stops both containers before sealing and a
//!   clone of that sealed snapshot that boots the same committed row and cache value, and a
//!   clear-`BTRFS_SUBVOL_RDONLY`-then-destroy teardown through the same driver instance.
//! - **Non-Btrfs host** (the XFS developer host): `BtrfsEngine` fails closed with
//!   [`EngineError::BtrfsRequired`] naming the `mount -o user_subvol_rm_allowed,noatime`
//!   remediation, before any snapshot, world, or control directory exists. One diagnostic line
//!   states that the kernel suite runs on the Btrfs VM.
//!
//! Every host asserts something: the live suite is never skipped and never ignored, and the
//! non-Btrfs branch is a real contract assertion (the production engine's fail-closed boot gate).

pub mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver, ProbeFacts, mount_hint};
use slipfork_engine::{BtrfsEngine, current_uid_gid};
use slipfork_testkit::poll_until;
use slipfork_world::WorldTarget;
use slipfork_world::rootfs::BackingServiceKey;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServiceKind,
    ServiceRuntime, ServiceSpec, ServiceState, SnapshotId, StateSubdirMount, WorldEngine,
    WorldHandle, WorldMount, WorldSpec,
};
use support::{PG_DATA, container_env, log_text, pg_state, world_env};

const PG_PATH: &str =
    "/usr/lib/postgresql/18/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const REDIS_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const REDIS_DATA: &str = "/data";
/// In-world port both clones must own simultaneously (the collision the world `netns` prevents).
const PG_PORT: u16 = 5432;
/// In-world cache port both clones must own simultaneously.
const REDIS_PORT: u16 = 6379;
/// A fixed SysV shared-memory key: two private IPC namespaces may both hold it.
const SHM_KEY: i64 = 90000;
/// `IPC_CREAT | IPC_EXCL`: create the key or fail `EEXIST` inside the caller's IPC namespace.
const SHM_EXCLUSIVE: &str = "0o3000";
/// `IPC_CREAT`: create-or-attach the key inside the caller's IPC namespace.
const SHM_CREATE: &str = "0o1000";
/// No create flag: attach an existing key or fail `ENOENT` inside the caller's IPC namespace.
const SHM_ATTACH: &str = "0";
/// `IPC_RMID`: remove the segment named by the `shmid` inside the caller's IPC namespace.
const IPC_RMID: i64 = 0;
/// Bound on a container reaching the terminal state a test expects (`initdb` finishing).
const SERVICE_STATE_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound on one `psql`/`redis-cli` query answering after the engine's own readiness gate passed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Marked values planted in each world's `base_env`: a container must receive neither the value
/// nor the name (`ServiceSpec::env` is a container's complete declared environment).
const WORLD_SECRET_MARKER: &str = "slipfork-live-world-secret-marker";
const WORLD_PLAIN_MARKER: &str = "slipfork-live-world-plain-marker";

#[test]
fn btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation() {
    let root = support::disk_tempdir("btrfs-live");
    let state_dir = root.path().join("state");
    fs::create_dir_all(&state_dir).expect("state dir");

    // The one `statfs` probe (and the one real driver) this suite performs: `0x9123683e` is
    // `BTRFS_SUPER_MAGIC`. The engine and the teardown share this same driver instance.
    let driver = KernelBtrfsDriver::new();
    let probe = driver
        .probe_facts(&state_dir)
        .expect("statfs the state dir");
    if probe.fstype == BTRFS_SUPER_MAGIC {
        run_kernel_suite(root.path(), &state_dir, driver);
    } else {
        assert_fail_closed_contract(&state_dir, &probe);
    }
}

/// A non-Btrfs host: the production engine refuses the boot gate with the exact remediation.
fn assert_fail_closed_contract(state_dir: &Path, probe: &ProbeFacts) {
    let actual_fstype = probe.fstype;
    let (uid, gid) = current_uid_gid();
    let err = BtrfsEngine::new(
        state_dir,
        uid,
        gid,
        env!("CARGO_BIN_EXE_slipfork-world-init"),
        KernelBtrfsDriver::new(),
        support::inherited_env(),
    )
    .expect_err("a non-Btrfs state dir must refuse the engine");
    match err {
        EngineError::BtrfsRequired {
            path,
            actual_fstype: reported,
            mount_hint: hint,
        } => {
            assert_eq!(path, state_dir, "the refusal names the state directory");
            assert_eq!(
                reported, actual_fstype,
                "the refusal reports the f_type statfs observed"
            );
            assert_eq!(hint, mount_hint(state_dir));
            assert!(
                hint.contains("mount -o user_subvol_rm_allowed,noatime"),
                "the refusal names the exact mount remediation: {hint}"
            );
        }
        other => panic!("expected BtrfsRequired, got {other:?}"),
    }
    for dir in ["worlds", "snapshots", "ctl"] {
        assert!(
            !state_dir.join(dir).exists(),
            "a refused engine creates no {dir}/ state"
        );
    }
    eprintln!(
        "critical_btrfs_live: probed {} (fstype 0x{actual_fstype:x}) is not BTRFS_SUPER_MAGIC \
         (0x9123_683e); the real kernel suite (RO snapshot, two writable clones, postgres:18 + \
         redis:8 on the same in-world port, SysV-IPC isolation, clear-RO-then-destroy) runs on the \
         Btrfs DO VM. Here the fail-closed BtrfsRequired contract is asserted.",
        probe.probed_path.display()
    );
}

/// A Btrfs host: the real ioctl, namespace, and `crun` path.
fn run_kernel_suite(root: &Path, state_dir: &Path, driver: KernelBtrfsDriver) {
    assert!(
        support::pinned_crun().is_file(),
        "the pinned static crun is a real input of the live suite (sha256-pinned, downloaded once)"
    );
    let (uid, gid) = current_uid_gid();
    let engine = BtrfsEngine::new(
        state_dir,
        uid,
        gid,
        env!("CARGO_BIN_EXE_slipfork-world-init"),
        driver,
        support::inherited_env(),
    )
    .expect("BtrfsEngine over KernelBtrfsDriver");
    let rootfs = backing_service_rootfs(state_dir);

    // 1. One real read-only snapshot of the base-seed directory.
    let src = root.join("state-src");
    fs::create_dir_all(&src).expect("state-src");
    fs::write(src.join("seed.txt"), b"ancestor\n").expect("seed");
    engine
        .create_snapshot(&src, &SnapshotId("base".to_owned()))
        .expect("seal the base snapshot");
    let sealed = state_dir.join("snapshots").join("base");
    assert_eq!(
        fs::read_to_string(sealed.join("seed.txt")).expect("sealed payload"),
        "ancestor\n"
    );
    assert!(
        fs::write(sealed.join("must-fail"), b"nope").is_err(),
        "BTRFS_SUBVOL_RDONLY must make the sealed snapshot unwritable"
    );

    // 2. Two writable clones of that one read-only snapshot.
    let seed_dumps = root.join("seed-dumps");
    fs::create_dir_all(&seed_dumps).expect("seed-dumps");
    let world_a = engine
        .clone_world(&world_spec(
            WorldTarget::Canonical,
            &rootfs,
            "base",
            root.join("repo-a"),
            root.join("home-a"),
            &seed_dumps,
        ))
        .expect("clone world a");
    let world_b = engine
        .clone_world(&world_spec(
            WorldTarget::UserFork("sib".to_owned()),
            &rootfs,
            "base",
            root.join("repo-b"),
            root.join("home-b"),
            &seed_dumps,
        ))
        .expect("clone world b");

    // 3. `postgres:18` and `redis:8` run in **both** clones on the same in-world ports.
    let (pg_a, redis_a) = init_and_start(&engine, &world_a, "a");
    let (pg_b, redis_b) = init_and_start(&engine, &world_b, "b");
    for (world, pg, redis, label) in [
        (&world_a, &pg_a, &redis_a, "a"),
        (&world_b, &pg_b, &redis_b, "b"),
    ] {
        assert_eq!(
            pg.state(),
            ServiceState::Running,
            "postgres in clone {label}"
        );
        assert_eq!(
            redis.state(),
            ServiceState::Running,
            "redis in clone {label}"
        );
        // `spawn_service` returned only after `dial_world_tcp` accepted on the shared port, so the
        // engine's own readiness path already proved both clones own 5432/6379 simultaneously.
        let stream = engine
            .dial_world_tcp(world, PG_PORT)
            .unwrap_or_else(|err| panic!("clone {label} must own in-world :{PG_PORT}: {err}"));
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let stream = engine
            .dial_world_tcp(world, REDIS_PORT)
            .unwrap_or_else(|err| panic!("clone {label} must own in-world :{REDIS_PORT}: {err}"));
        let _ = stream.shutdown(std::net::Shutdown::Both);
        assert_eq!(
            query_until(|| psql(&engine, pg, "select 1")),
            "1",
            "postgres:18 in clone {label} must answer a real query"
        );
        assert_eq!(
            query_until(|| redis_cli(&engine, redis, &["ping"])),
            "PONG",
            "redis:8 in clone {label} must answer PING"
        );
        // The service images are the digest-pinned real ones, not a stub.
        assert!(
            query_until(|| redis_cli(&engine, redis, &["info", "server"]))
                .contains("redis_version:8."),
            "clone {label} must run redis:8"
        );
        assert!(
            query_until(|| psql(&engine, pg, "select current_setting('server_version')"))
                .starts_with("18."),
            "clone {label} must run postgres:18"
        );
    }

    // 3b. The world component environment never reaches a container: each world `base_env` carries
    //     a marked secret and a marked plain variable (`world_spec`), yet `exec` inside the
    //     postgres container sees neither value nor name, only its own declared `spec.env`.
    let container_env_dump = container_exec(&engine, &pg_a, &["sh", "-c", "env"]);
    assert_eq!(
        container_env_dump.exit_code, 0,
        "env inside the postgres container: {}",
        container_env_dump.stderr
    );
    for needle in [
        WORLD_SECRET_MARKER,
        WORLD_PLAIN_MARKER,
        "WORLD_SECRET_MARKER",
        "WORLD_PLAIN_MARKER",
    ] {
        assert!(
            !container_env_dump.stdout.contains(needle),
            "the container exec env leaked `{needle}` from the world component environment:\n{}",
            container_env_dump.stdout
        );
    }
    for needle in [format!("PATH={PG_PATH}"), format!("PGDATA={PG_DATA}")] {
        assert!(
            container_env_dump.stdout.contains(&needle),
            "the container must still receive its own `{needle}`:\n{}",
            container_env_dump.stdout
        );
    }
    // A container's bundle env is fixed at spawn: the update is refused before the container is
    // touched, and the running service stays up.
    let err = engine
        .update_service_env(&pg_a, &ExecEnv::default())
        .expect_err("a container's bundle env is fixed at spawn");
    assert!(
        matches!(err, EngineError::ContainerEnvFixed { .. }),
        "expected ContainerEnvFixed, got {err:?}"
    );
    assert_eq!(
        pg_a.state(),
        ServiceState::Running,
        "a refused env update must not stop the container"
    );

    // 4. SysV IPC isolation across the clones: `shmget(2)` for one fixed key from both worlds. Each
    //    `exec_in_world` enters with `nsenter --ipc`, so this proves the private IPC namespace the
    //    world PID 1 was created with (without it, the second clone would see `EEXIST`).
    let created_a = shm_probe(&engine, &world_a, SHM_EXCLUSIVE);
    assert_eq!(
        created_a.exit_code, 0,
        "clone A must create SysV key {SHM_KEY}: {}",
        created_a.stderr
    );
    let created_b = shm_probe(&engine, &world_b, SHM_EXCLUSIVE);
    assert_eq!(
        created_b.exit_code, 0,
        "clone B must create the same key in its own IPC namespace: {}",
        created_b.stderr
    );
    let duplicated_in_a = shm_probe(&engine, &world_a, SHM_EXCLUSIVE);
    assert_eq!(
        duplicated_in_a.exit_code, 17,
        "a duplicate key inside clone A must fail EEXIST, proving A's segment is real: {}",
        duplicated_in_a.stdout
    );
    let attached_b = shm_probe(&engine, &world_b, SHM_CREATE);
    assert_eq!(
        attached_b.exit_code, 0,
        "clone B must attach its own key {SHM_KEY}: {}",
        attached_b.stderr
    );
    //    Independence is proved by removal, not by comparing `shmid` values: a fresh IPC
    //    namespace numbers its first segment 0 in both clones. Instead, remove clone A's segment
    //    (`shmctl(..., IPC_RMID)`) and observe that the same key is then `ENOENT` (2) in A while
    //    clone B still attaches its own live segment.
    let created_a_shmid: i64 = created_a
        .stdout
        .trim()
        .parse()
        .expect("clone A's shmget must print its shmid");
    let removed_a = shm_remove(&engine, &world_a, created_a_shmid);
    assert_eq!(
        removed_a.exit_code, 0,
        "clone A must remove SysV segment {created_a_shmid}: {}",
        removed_a.stderr
    );
    let gone_in_a = shm_probe(&engine, &world_a, SHM_ATTACH);
    assert_eq!(
        gone_in_a.exit_code, 2,
        "clone A's key must be gone (ENOENT) after IPC_RMID: {}",
        gone_in_a.stdout
    );
    let still_in_b = shm_probe(&engine, &world_b, SHM_ATTACH);
    assert_eq!(
        still_in_b.exit_code, 0,
        "clone B must still own its own segment for the same key after A removed its own: {}",
        still_in_b.stderr
    );
    assert_eq!(
        still_in_b.stdout.trim(),
        created_b.stdout.trim(),
        "clone B attaches the segment it created, untouched by clone A's removal"
    );

    // 5. A read-only dump volume: a real `pg_dump`, a real `EROFS` write refusal, a real
    //    `pg_restore` (the `ADR-0009` §3.1 read-only-mount contract, end to end in a container).
    assert_eq!(
        exec_ok_until(|| container_exec(
            &engine,
            &pg_a,
            &[
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-Atc",
                "create table seed(v text)",
            ],
        ))
        .exit_code,
        0,
        "create the dump source table"
    );
    assert_eq!(
        exec_ok_until(|| container_exec(
            &engine,
            &pg_a,
            &[
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-Atc",
                "insert into seed values ('restored')",
            ],
        ))
        .exit_code,
        0,
        "seed one row"
    );
    let dump = container_exec(
        &engine,
        &pg_a,
        &[
            "sh",
            "-c",
            "pg_dump -h 127.0.0.1 -U postgres -d postgres -Fc | base64 -w0",
        ],
    );
    assert_eq!(dump.exit_code, 0, "pg_dump stderr: {}", dump.stderr);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(dump.stdout.trim())
        .expect("pg_dump must stream canonical base64");
    assert!(
        bytes.len() > 100,
        "pg_dump must produce a real archive (got {} bytes)",
        bytes.len()
    );
    let dump_host = seed_dumps.join("seed.pgdump");
    fs::write(&dump_host, &bytes).expect("write verified dump");

    let client = engine
        .spawn_service(
            &world_a,
            &oci_service(
                "pg-client",
                BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                &["sh", "-c", "echo pg-client-ready; exec sleep infinity"],
                container_env(PG_PATH),
                Vec::new(),
                vec![WorldMount {
                    host_path: dump_host,
                    guest_path: PathBuf::from("/dumps/seed.pgdump"),
                    mode: MountMode::ReadOnly,
                }],
                None,
            ),
        )
        .expect("spawn read-only client container");
    let exec_ready = poll_until(Duration::from_secs(30), || {
        let out = container_exec(&engine, &client, &["sleep", "0"]);
        if out.exit_code == 0 {
            Ok(())
        } else {
            Err(format!("crun exec exited {}", out.exit_code))
        }
    });
    assert!(
        exec_ready.is_ok(),
        "pg-client never accepted exec: {exec_ready:?}"
    );
    let visible = container_exec(&engine, &client, &["ls", "-l", "/dumps/seed.pgdump"]);
    assert_eq!(visible.exit_code, 0, "stderr: {}", visible.stderr);
    assert!(visible.stdout.contains("seed.pgdump"));
    let write = container_exec(
        &engine,
        &client,
        &["sh", "-c", "echo nope >> /dumps/seed.pgdump"],
    );
    assert_ne!(write.exit_code, 0, "a read-only mount must reject writes");
    assert!(
        write.stderr.to_lowercase().contains("read-only")
            || write.stderr.to_lowercase().contains("readonly"),
        "expected EROFS, got stderr: {}",
        write.stderr
    );
    assert_eq!(
        exec_ok_until(|| container_exec(
            &engine,
            &pg_a,
            &[
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-Atc",
                "create database restored",
            ],
        ))
        .exit_code,
        0
    );
    let restore = container_exec(
        &engine,
        &client,
        &[
            "pg_restore",
            "-h",
            "127.0.0.1",
            "-U",
            "postgres",
            "-d",
            "restored",
            "/dumps/seed.pgdump",
        ],
    );
    assert_eq!(
        restore.exit_code, 0,
        "pg_restore stderr: {}",
        restore.stderr
    );
    let restored = container_exec(
        &engine,
        &client,
        &[
            "psql",
            "-h",
            "127.0.0.1",
            "-U",
            "postgres",
            "-d",
            "restored",
            "-Atc",
            "select v from seed",
        ],
    );
    assert_eq!(restored.stdout.trim(), "restored");
    engine.stop_service(&client).expect("stop client");

    // `stop_service` keeps the captured container logs and the typed refusal afterwards.
    assert_eq!(client.state(), ServiceState::Stopped);
    let client_logs = engine
        .service_logs(&world_a, "pg-client")
        .expect("logs survive stop");
    assert!(
        !log_text(&client_logs).is_empty(),
        "pg-client container logs must survive stop_service"
    );
    let err = engine
        .exec_in_service(
            &client,
            &["sleep".to_owned(), "0".to_owned()],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect_err("a stopped container must refuse exec");
    assert!(
        matches!(err, EngineError::ServiceNotRunning { .. }),
        "expected ServiceNotRunning, got {err:?}"
    );
    let err = engine
        .service_entry_command(&client)
        .expect_err("a stopped container must refuse entry");
    assert!(
        matches!(err, EngineError::ServiceNotRunning { .. }),
        "expected ServiceNotRunning, got {err:?}"
    );

    // The pre-freeze state a clone of the sealed snapshot must inherit.
    assert_eq!(
        query_until(|| psql(&engine, &pg_a, "select v from seed")),
        "restored",
        "clone A must hold its committed row before the freeze"
    );
    assert_eq!(
        query_until(|| redis_cli(&engine, &redis_a, &["set", "clone-key", "clone-value"])),
        "OK",
        "clone A must persist a redis value before the freeze"
    );

    // 6. Freeze stops both writers before sealing, then a clone of the sealed snapshot boots the
    //    same postgres:18/redis:8 state independently (the state lineage, end to end).
    engine
        .freeze_world(&world_a, &SnapshotId("after-clone".to_owned()))
        .expect("seal clone A");
    assert_eq!(
        pg_a.state(),
        ServiceState::Stopped,
        "freeze_world must stop postgres before sealing the subvolume"
    );
    assert_eq!(
        redis_a.state(),
        ServiceState::Stopped,
        "freeze_world must stop redis before sealing the subvolume"
    );
    let sealed_world = engine
        .clone_world(&world_spec(
            WorldTarget::UserFork("sealed".to_owned()),
            &rootfs,
            "after-clone",
            root.join("repo-sealed"),
            root.join("home-sealed"),
            &seed_dumps,
        ))
        .expect("clone the sealed snapshot");
    let (pg_sealed, redis_sealed) = start_services(&engine, &sealed_world, "sealed");
    assert_eq!(
        query_until(|| psql(&engine, &pg_sealed, "select v from seed")),
        "restored",
        "the sealed clone must inherit clone A's committed postgres row"
    );
    assert_eq!(
        query_until(|| redis_cli(&engine, &redis_sealed, &["get", "clone-key"])),
        "clone-value",
        "the sealed clone must inherit clone A's persisted redis value"
    );
    engine
        .destroy_world(sealed_world)
        .expect("destroy the sealed clone");

    // 7. Clear-RO-then-destroy teardown: destroy both writable clones, then destroy the read-only
    //    snapshots through the same production driver. Each destroy succeeds only because the
    //    driver clears `BTRFS_SUBVOL_RDONLY` first (a read-only subvolume cannot be removed).
    engine.destroy_world(world_a).expect("destroy clone a");
    engine.destroy_world(world_b).expect("destroy clone b");
    let snapshots = state_dir.join("snapshots");
    for name in ["after-clone", "base"] {
        driver
            .destroy_subvolume(&snapshots, name)
            .unwrap_or_else(|err| panic!("clear-RO-then-destroy of '{name}' failed: {err}"));
        assert!(!snapshots.join(name).exists(), "'{name}' must be gone");
    }
    assert!(
        driver
            .list_children(&snapshots)
            .expect("list snapshots")
            .is_empty(),
        "no sealed subvolume survives the teardown"
    );
    assert!(
        !state_dir.join("worlds").exists()
            || fs::read_dir(state_dir.join("worlds"))
                .expect("read_dir worlds")
                .next()
                .is_none(),
        "no world subvolume survives the teardown"
    );
    assert!(
        !state_dir.join("ctl").exists()
            || fs::read_dir(state_dir.join("ctl"))
                .expect("read_dir ctl")
                .next()
                .is_none(),
        "no control directory survives the teardown"
    );
}

/// The real content-addressed `L0` tree plus the two digest-pinned backing-service images.
fn backing_service_rootfs(state_dir: &Path) -> WorldRootfs {
    let mut rootfs = support::engine_rootfs(state_dir);
    rootfs.service_images = BTreeMap::from([
        (
            BackingServiceKey::Postgres {
                version: "18".to_owned(),
            },
            support::oci_rootfs(
                "postgres:18",
                &["/var/lib/postgresql/data/", "/dumps/seed.pgdump"],
            ),
        ),
        (
            BackingServiceKey::Redis {
                version: "8".to_owned(),
            },
            support::oci_rootfs("redis:8", &["/data/"]),
        ),
    ]);
    rootfs
}

fn world_spec(
    target: WorldTarget,
    rootfs: &WorldRootfs,
    snapshot: &str,
    repo: PathBuf,
    home: PathBuf,
    seed_dumps: &Path,
) -> WorldSpec {
    fs::create_dir_all(&repo).expect("repo");
    WorldSpec {
        target,
        rootfs: rootfs.clone(),
        snapshot: SnapshotId(snapshot.to_owned()),
        mounts: vec![
            WorldMount {
                host_path: repo.clone(),
                guest_path: repo,
                mode: MountMode::ReadWrite,
            },
            WorldMount {
                host_path: seed_dumps.to_path_buf(),
                guest_path: seed_dumps.to_path_buf(),
                mode: MountMode::ReadOnly,
            },
        ],
        base_env: {
            let mut env = world_env(&home);
            env.set_secret("WORLD_SECRET_MARKER", WORLD_SECRET_MARKER)
                .expect("bind the marked world secret");
            env.set("WORLD_PLAIN_MARKER", WORLD_PLAIN_MARKER)
                .expect("bind the marked world variable");
            env
        },
        local_ingress: None,
        boot: BootLineage::default(),
    }
}

fn oci_service(
    id: &str,
    image: BackingServiceKey,
    argv: &[&str],
    env: ExecEnv,
    state_mounts: Vec<StateSubdirMount>,
    readonly_mounts: Vec<WorldMount>,
    readiness_port: Option<u16>,
) -> ServiceSpec {
    ServiceSpec {
        id: id.to_owned(),
        kind: ServiceKind::BackingService,
        runtime: ServiceRuntime::OciContainer {
            image,
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            state_mounts,
            readonly_mounts,
        },
        env,
        readiness_port,
    }
}

/// The `postgres:18` container's own declared environment: the image `PATH` and the `PGDATA` the
/// image declares. Never the world component env.
fn pg_env() -> ExecEnv {
    let mut env = container_env(PG_PATH);
    env.set("PGDATA", PG_DATA).expect("bind PGDATA");
    env
}

fn redis_state() -> Vec<StateSubdirMount> {
    vec![StateSubdirMount {
        host_subdir: PathBuf::from("redis/cache"),
        container_path: PathBuf::from(REDIS_DATA),
    }]
}

/// Create postgres's data directory once (`initdb`), then exit.
fn init_postgres(engine: &BtrfsEngine, world: &WorldHandle, label: &str) {
    let init = engine
        .spawn_service(
            world,
            &oci_service(
                "pg-init",
                BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                &[
                    "initdb",
                    "-D",
                    PG_DATA,
                    "-U",
                    "postgres",
                    "--auth=trust",
                    "--locale=C",
                    "--encoding=UTF8",
                    "--no-instructions",
                ],
                pg_env(),
                pg_state(),
                Vec::new(),
                None,
            ),
        )
        .expect("spawn initdb container");
    let finished = poll_until(SERVICE_STATE_TIMEOUT, || {
        let state = init.state();
        if state == ServiceState::Exited {
            Ok(())
        } else {
            Err(format!("last observed {state:?}"))
        }
    });
    assert!(
        finished.is_ok(),
        "clone {label} initdb did not finish: {finished:?}"
    );
    let logs = log_text(&engine.service_logs(world, "pg-init").expect("init logs"));
    assert!(
        !logs.contains("initdb: error"),
        "clone {label} initdb failed:\n{logs}"
    );
}

/// Start `postgres:18` and `redis:8` in `world` over an already-initialized state tree.
fn start_services(
    engine: &BtrfsEngine,
    world: &WorldHandle,
    label: &str,
) -> (ServiceHandle, ServiceHandle) {
    let pg = engine
        .spawn_service(
            world,
            &oci_service(
                "postgres",
                BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                &[
                    "postgres",
                    "-D",
                    PG_DATA,
                    "-c",
                    "listen_addresses=127.0.0.1",
                    "-c",
                    "port=5432",
                    "-c",
                    "unix_socket_directories=/tmp",
                ],
                pg_env(),
                pg_state(),
                Vec::new(),
                Some(PG_PORT),
            ),
        )
        .unwrap_or_else(|error| {
            let logs = engine
                .service_logs(world, "postgres")
                .map(|logs| {
                    logs.snapshot()
                        .iter()
                        .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
                        .collect::<String>()
                })
                .unwrap_or_default();
            panic!(
                "clone {label}: spawn postgres: {error}\n--- retained postgres logs ---\n{logs}"
            );
        });
    let redis = engine
        .spawn_service(
            world,
            &oci_service(
                "redis",
                BackingServiceKey::Redis {
                    version: "8".to_owned(),
                },
                &[
                    "redis-server",
                    "--port",
                    "6379",
                    "--bind",
                    "127.0.0.1",
                    "--dir",
                    REDIS_DATA,
                    "--appendonly",
                    "yes",
                    "--protected-mode",
                    "no",
                ],
                container_env(REDIS_PATH),
                redis_state(),
                Vec::new(),
                Some(REDIS_PORT),
            ),
        )
        .unwrap_or_else(|error| panic!("clone {label}: spawn redis: {error}"));
    (pg, redis)
}

/// Initialize a fresh world once, then start both services in it.
fn init_and_start(
    engine: &BtrfsEngine,
    world: &WorldHandle,
    label: &str,
) -> (ServiceHandle, ServiceHandle) {
    init_postgres(engine, world, label);
    start_services(engine, world, label)
}

fn container_exec(engine: &BtrfsEngine, service: &ServiceHandle, argv: &[&str]) -> ExecOutput {
    engine
        .exec_in_service(
            service,
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("exec inside container")
}

/// `shmget(2)` for one fixed key inside `world`'s private System V IPC namespace.
///
/// `exec_in_world` enters with `nsenter --ipc`, so the syscall runs in the namespace the world PID 1
/// was created with; the script exits with the raw `errno` on failure (17 = `EEXIST`).
fn shm_probe(engine: &BtrfsEngine, world: &WorldHandle, flags: &str) -> ExecOutput {
    let script = format!(
        "import ctypes\n\
         libc = ctypes.CDLL(None, use_errno=True)\n\
         libc.shmget.restype = ctypes.c_int\n\
         shmid = libc.shmget({SHM_KEY}, 4096, {flags})\n\
         if shmid < 0:\n\
         \x20   raise SystemExit(ctypes.get_errno())\n\
         print(shmid)\n"
    );
    engine
        .exec_in_world(
            world,
            &["python3".to_owned(), "-c".to_owned(), script],
            Path::new("/"),
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("shmget inside world")
}

/// `shmctl(shmid, IPC_RMID)` inside `world`'s private System V IPC namespace.
fn shm_remove(engine: &BtrfsEngine, world: &WorldHandle, shmid: i64) -> ExecOutput {
    let script = format!(
        "import ctypes\n\
         libc = ctypes.CDLL(None, use_errno=True)\n\
         libc.shmctl.restype = ctypes.c_int\n\
         rc = libc.shmctl({shmid}, {IPC_RMID}, None)\n\
         if rc != 0:\n\
         \x20   raise SystemExit(ctypes.get_errno())\n\
         print(\"removed\")\n"
    );
    engine
        .exec_in_world(
            world,
            &["python3".to_owned(), "-c".to_owned(), script],
            Path::new("/"),
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("shmctl inside world")
}

fn psql(engine: &BtrfsEngine, service: &ServiceHandle, sql: &str) -> String {
    let out = container_exec(
        engine,
        service,
        &[
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "5432",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-Atc",
            sql,
        ],
    );
    out.stdout.trim().to_owned()
}

fn redis_cli(engine: &BtrfsEngine, service: &ServiceHandle, argv: &[&str]) -> String {
    let mut full = vec!["redis-cli", "-p", "6379"];
    full.extend_from_slice(argv);
    container_exec(engine, service, &full)
        .stdout
        .trim()
        .to_owned()
}

/// Bounded retry for a statement whose success is a container exec exiting zero.
fn exec_ok_until(mut run: impl FnMut() -> ExecOutput) -> ExecOutput {
    let mut last = None;
    let outcome = poll_until(QUERY_TIMEOUT, || {
        let out = run();
        let ok = out.exit_code == 0;
        let diagnostic = format!("exited {}: {}", out.exit_code, out.stderr);
        last = Some(out);
        if ok { Ok(()) } else { Err(diagnostic) }
    });
    assert!(
        outcome.is_ok(),
        "statement never succeeded within {QUERY_TIMEOUT:?}: {outcome:?}"
    );
    last.expect("the probe always runs at least once")
}

/// Bounded retry for a query that must eventually answer; the failure names the last attempt.
fn query_until(mut query: impl FnMut() -> String) -> String {
    let mut last = String::new();
    let outcome = poll_until(QUERY_TIMEOUT, || {
        last = query();
        if last.is_empty() {
            Err("empty answer".to_owned())
        } else {
            Ok(())
        }
    });
    assert!(
        outcome.is_ok(),
        "no answer within {QUERY_TIMEOUT:?}: {outcome:?}"
    );
    last
}
