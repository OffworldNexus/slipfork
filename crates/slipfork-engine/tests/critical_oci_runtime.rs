//! Protected live test: daemonless `crun` backing-service containers (`ADR-0009` §2–§3).
//!
//! Boots `postgres:18` and `redis:8` from their digest-pinned cached OCI rootfs inside a real
//! unprivileged `unshare` world, dials `127.0.0.1:5432/6379` from another process in that world's
//! network namespace, executes `psql`/`redis-cli` **inside** each container, and proves a
//! `FICLONE` clone of the sealed state boots cleanly. Missing Docker, `crun`, or network fails
//! loudly with the actionable diagnostic — the test never skips.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_engine::rootfs::BackingServiceKey;
use slipfork_engine::{
    EngineError, ExecEnv, InheritedVar, MountMode, ReflinkNetnsEngine, ServiceHandle, ServiceLogs,
    ServiceRuntime, ServiceSpec, ServiceState, SnapshotId, StateSubdirMount, WorldEngine,
    WorldHandle, WorldMount, WorldRootfs, WorldSpec, current_uid_gid,
};
use slipfork_testkit::poll_until;
use slipfork_world::WorldTarget;

const PG_PATH: &str =
    "/usr/lib/postgresql/18/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const REDIS_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const PG_DATA: &str = "/var/lib/postgresql/data";
const REDIS_DATA: &str = "/data";
/// Bound on a container reaching the terminal state a test expects (`initdb` finishing).
const SERVICE_STATE_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound on `crun exec` accepting a command after the container started.
const EXEC_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Follow-up bound on `postgres:18` answering a real query.
///
/// `spawn_service` waits up to the engine's 60 s `OCI_SERVICE_READY_TIMEOUT`
/// (`crates/slipfork-engine/src/reflink_netns.rs`) for the `ServiceSpec::readiness_port` listener
/// to accept, and a TCP-accepting postgres can still be finishing startup recovery. This 20 s
/// caller-side follow-up therefore runs after that 60 s window, not inside it. It is a stopgap,
/// not a second readiness contract.
///
/// This stopgap and `PG_READY_TIMEOUT` are removed when the engine-owned
/// `ServiceSpec.readiness: ReadinessProbe` (`ADR-0002` §2) lands in `M3.13`–`M3.15`:
/// `spawn_service` then returns only once in-container `psql`/`redis-cli` probes answer, so no
/// caller polls a service itself.
const PG_READY_TIMEOUT: Duration = Duration::from_secs(20);

fn root_dir(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in label.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("sfoci")
        .join(format!("{:08x}-{}", hash & 0xffff_ffff, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create OCI test root");
    dir
}

fn snap(label: &str) -> SnapshotId {
    SnapshotId(label.to_owned())
}

fn env_with_path(path: &str) -> ExecEnv {
    let mut env = ExecEnv::default();
    env.inherit(InheritedVar::Path, path);
    // The minimal service images carry no locale archive; `LANG=C.UTF-8` from a developer shell
    // would make Redis `setlocale` fail before it binds the port.
    env.inherit(InheritedVar::Lang, "C");
    env
}

fn world_env(home: &Path) -> ExecEnv {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(InheritedVar::Path, PATH_FOR_WORLD);
    env
}

const PATH_FOR_WORLD: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

struct Harness {
    root: PathBuf,
    engine: ReflinkNetnsEngine,
    snapshot: SnapshotId,
    home: PathBuf,
    repo: PathBuf,
    rootfs: WorldRootfs,
}

impl Harness {
    fn new(label: &str) -> Self {
        let root = root_dir(label);
        let (uid, gid) = current_uid_gid();
        let engine = ReflinkNetnsEngine::with_world_init_program(
            root.join("engine"),
            uid,
            gid,
            env!("CARGO_BIN_EXE_slipfork-world-init"),
        );
        let src = root.join("state-src");
        fs::create_dir_all(&src).expect("state-src");
        fs::write(src.join("seed.txt"), b"ancestor\n").expect("seed");
        let snapshot = snap("base");
        engine
            .create_snapshot(&src, &snapshot)
            .expect("base snapshot");
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&repo).expect("repo");
        let mut rootfs = support::fixture_rootfs();
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
        Self {
            root,
            engine,
            snapshot,
            home,
            repo,
            rootfs,
        }
    }

    fn clone_world(&self, snapshot: &SnapshotId, target: WorldTarget) -> WorldHandle {
        self.engine
            .clone_world(&WorldSpec {
                target,
                rootfs: self.rootfs.clone(),
                snapshot: snapshot.clone(),
                mounts: vec![WorldMount {
                    host_path: self.repo.clone(),
                    guest_path: self.repo.clone(),
                    mode: MountMode::ReadWrite,
                }],
                base_env: world_env(&self.home),
                local_ingress: None,
            })
            .expect("clone OCI world")
    }

    #[allow(clippy::too_many_arguments)]
    fn oci_service(
        &self,
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

    fn pg_state() -> Vec<StateSubdirMount> {
        vec![StateSubdirMount {
            host_subdir: PathBuf::from("pgdata/main"),
            container_path: PathBuf::from(PG_DATA),
        }]
    }

    fn redis_state() -> Vec<StateSubdirMount> {
        vec![StateSubdirMount {
            host_subdir: PathBuf::from("redis/cache"),
            container_path: PathBuf::from(REDIS_DATA),
        }]
    }

    fn wait_for_state(&self, handle: &ServiceHandle, expected: ServiceState) {
        let observed = poll_until(SERVICE_STATE_TIMEOUT, || {
            let state = handle.state();
            if state == expected {
                Ok(())
            } else {
                Err(format!("last observed {state:?}"))
            }
        });
        if let Err(last) = observed {
            let logs = self
                .engine
                .service_logs(handle.world(), handle.id())
                .map(|logs| log_text(&logs))
                .unwrap_or_else(|error| format!("<logs unavailable: {error}>"));
            panic!(
                "service '{}' did not reach {expected:?} within {SERVICE_STATE_TIMEOUT:?} ({last}); logs:\n{logs}",
                handle.id()
            );
        }
    }

    fn init_postgres(&self, world: &WorldHandle) {
        let spec = self.oci_service(
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
            env_with_path(PG_PATH),
            Self::pg_state(),
            Vec::new(),
            None,
        );
        let init = self
            .engine
            .spawn_service(world, &spec)
            .expect("spawn initdb container");
        self.wait_for_state(&init, ServiceState::Exited);
        let logs = log_text(
            &self
                .engine
                .service_logs(world, "pg-init")
                .expect("init logs"),
        );
        assert!(
            !logs.contains("initdb: error"),
            "initdb failed inside the container, logs:\n{logs}"
        );
    }

    fn start_postgres(&self, world: &WorldHandle) -> ServiceHandle {
        self.engine
            .spawn_service(
                world,
                &self.oci_service(
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
                    env_with_path(PG_PATH),
                    Self::pg_state(),
                    Vec::new(),
                    Some(5432),
                ),
            )
            .unwrap_or_else(|error| {
                let logs = self
                    .engine
                    .service_logs(world, "postgres")
                    .map(|logs| log_text(&logs))
                    .unwrap_or_else(|error| format!("<logs unavailable: {error}>"));
                panic!("spawn postgres container: {error}\nlogs:\n{logs}")
            })
    }

    fn start_redis(&self, world: &WorldHandle) -> ServiceHandle {
        self.engine
            .spawn_service(
                world,
                &self.oci_service(
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
                    env_with_path(REDIS_PATH),
                    Self::redis_state(),
                    Vec::new(),
                    Some(6379),
                ),
            )
            .unwrap_or_else(|error| {
                let logs = self
                    .engine
                    .service_logs(world, "redis")
                    .map(|logs| log_text(&logs))
                    .unwrap_or_else(|error| format!("<logs unavailable: {error}>"));
                panic!("spawn redis container: {error}\nlogs:\n{logs}")
            })
    }

    /// Wait until `crun run` has written the container status that `crun exec` needs.
    fn wait_exec_ready(&self, service: &ServiceHandle) {
        let ready = poll_until(EXEC_READY_TIMEOUT, || {
            match self.engine.exec_in_service(
                service,
                &["sleep".to_owned(), "0".to_owned()],
                &ExecEnv::default(),
                &mut |_| {},
            ) {
                Ok(output) if output.exit_code == 0 => Ok(()),
                Ok(output) => Err(format!(
                    "exec exited {}: {}",
                    output.exit_code, output.stderr
                )),
                Err(error) => Err(error.to_string()),
            }
        });
        if let Err(last) = ready {
            panic!(
                "container '{}' never accepted exec within {EXEC_READY_TIMEOUT:?}: {last}",
                service.id()
            );
        }
    }

    /// The one canonical postgres readiness proof: one real `select 1` per probe attempt.
    ///
    /// A container whose listener already accepts TCP connections can still be finishing postgres
    /// startup recovery (`the database system is starting up`, psql exit 2), so poll boundedly and
    /// report the last captured stderr on timeout.
    fn wait_postgres_ready(&self, service: &ServiceHandle) {
        let ready = poll_until(PG_READY_TIMEOUT, || {
            let output = psql(self, service, "select 1");
            if output.exit_code == 0 && output.stdout.trim() == "1" {
                Ok(())
            } else {
                Err(format!(
                    "psql exit {}, stderr:\n{}",
                    output.exit_code, output.stderr
                ))
            }
        });
        if let Err(last) = ready {
            panic!(
                "postgres '{}' never answered `select 1` within {PG_READY_TIMEOUT:?}; last attempt: {last}",
                service.id()
            );
        }
    }

    fn exec_in_world(&self, world: &WorldHandle, script: &str) -> slipfork_engine::ExecOutput {
        self.engine
            .exec_in_world(
                world,
                &["sh".to_owned(), "-c".to_owned(), script.to_owned()],
                &self.repo,
                &ExecEnv::default(),
                &mut |_| {},
            )
            .expect("exec inside world")
    }
}

fn log_text(logs: &ServiceLogs) -> String {
    logs.snapshot()
        .iter()
        .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
        .collect()
}

fn psql(harness: &Harness, service: &ServiceHandle, sql: &str) -> slipfork_engine::ExecOutput {
    harness
        .engine
        .exec_in_service(
            service,
            &[
                "psql".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-p".to_owned(),
                "5432".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-d".to_owned(),
                "postgres".to_owned(),
                "-Atc".to_owned(),
                sql.to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("psql inside postgres container")
}

#[test]
fn postgres_and_redis_share_the_world_netns_and_survive_a_reflink_clone() {
    let harness = Harness::new("oci-runtime");
    let world = harness.clone_world(&harness.snapshot, WorldTarget::Canonical);
    harness.init_postgres(&world);
    let postgres = harness.start_postgres(&world);
    harness.wait_postgres_ready(&postgres);
    let redis = harness.start_redis(&world);
    assert_eq!(postgres.state(), ServiceState::Running);
    assert_eq!(redis.state(), ServiceState::Running);

    // Another process in the **same world netns** reaches both containers on 127.0.0.1.
    let probe = harness.exec_in_world(
        &world,
        "python3 - <<'PY'\n\
         import socket\n\
         for port in (5432, 6379):\n\
         \x20   s = socket.create_connection((\"127.0.0.1\", port), 5)\n\
         \x20   s.close()\n\
         \x20   print(\"OPEN\", port)\n\
         r = socket.create_connection((\"127.0.0.1\", 6379), 5)\n\
         r.sendall(b\"PING\\r\\n\")\n\
         print(r.recv(64).decode())\n\
         r.close()\n\
         PY\n",
    );
    assert_eq!(probe.exit_code, 0, "stderr: {}", probe.stderr);
    assert!(
        probe.stdout.contains("OPEN 5432"),
        "stdout: {}",
        probe.stdout
    );
    assert!(
        probe.stdout.contains("OPEN 6379"),
        "stdout: {}",
        probe.stdout
    );
    assert!(probe.stdout.contains("+PONG"), "stdout: {}", probe.stdout);

    let server_version = psql(
        &harness,
        &postgres,
        "select current_setting('server_version')",
    );
    assert_eq!(
        server_version.exit_code, 0,
        "stderr: {}",
        server_version.stderr
    );
    assert!(
        server_version.stdout.trim().starts_with("18."),
        "postgres:18 must report 18.x, got {}",
        server_version.stdout
    );
    assert_eq!(
        psql(&harness, &postgres, "create table t(v text)").exit_code,
        0
    );
    assert_eq!(
        psql(
            &harness,
            &postgres,
            "insert into t values ('survives-clone')"
        )
        .exit_code,
        0
    );
    let redis_ping = harness
        .engine
        .exec_in_service(
            &redis,
            &[
                "redis-cli".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-p".to_owned(),
                "6379".to_owned(),
                "ping".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("redis-cli inside redis container");
    assert_eq!(redis_ping.stdout.trim(), "PONG");
    assert_eq!(
        harness
            .engine
            .exec_in_service(
                &redis,
                &[
                    "redis-cli".to_owned(),
                    "-p".to_owned(),
                    "6379".to_owned(),
                    "set".to_owned(),
                    "clone-key".to_owned(),
                    "clone-value".to_owned(),
                ],
                &ExecEnv::default(),
                &mut |_| {},
            )
            .expect("redis set")
            .stdout
            .trim(),
        "OK"
    );

    // `stop_service` keeps the captured container logs and the typed refusal afterwards.
    harness
        .engine
        .stop_service(&postgres)
        .expect("stop postgres");
    assert_eq!(postgres.state(), ServiceState::Stopped);
    let logs = harness
        .engine
        .service_logs(&world, "postgres")
        .expect("logs survive stop");
    assert!(
        !log_text(&logs).is_empty(),
        "postgres container logs must survive stop_service"
    );
    let err = harness
        .engine
        .exec_in_service(
            &postgres,
            &["psql".to_owned(), "-c".to_owned(), "select 1".to_owned()],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect_err("a stopped container must refuse exec");
    assert!(
        matches!(err, EngineError::ServiceNotRunning { .. }),
        "expected ServiceNotRunning, got {err:?}"
    );
    let err = harness
        .engine
        .service_entry_command(&postgres)
        .expect_err("a stopped container must refuse entry");
    assert!(
        matches!(err, EngineError::ServiceNotRunning { .. }),
        "expected ServiceNotRunning, got {err:?}"
    );

    // Freeze stops the running containers before reflinking the state directory, then a clone of
    // the sealed snapshot boots the same postgres:18/redis:8 state independently.
    let postgres = harness.start_postgres(&world);
    harness
        .engine
        .freeze_world(&world, &snap("sealed"))
        .expect("freeze world with live containers");
    assert_eq!(postgres.state(), ServiceState::Stopped);
    assert_eq!(redis.state(), ServiceState::Stopped);

    let clone = harness.clone_world(&snap("sealed"), WorldTarget::UserFork("clone".to_owned()));
    let postgres_clone = harness.start_postgres(&clone);
    harness.wait_postgres_ready(&postgres_clone);
    let redis_clone = harness.start_redis(&clone);
    assert_eq!(
        psql(&harness, &postgres_clone, "select v from t")
            .stdout
            .trim(),
        "survives-clone"
    );
    let value = harness
        .engine
        .exec_in_service(
            &redis_clone,
            &[
                "redis-cli".to_owned(),
                "-p".to_owned(),
                "6379".to_owned(),
                "get".to_owned(),
                "clone-key".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("redis get in clone");
    assert_eq!(value.stdout.trim(), "clone-value");

    harness.engine.destroy_world(clone).expect("destroy clone");
    harness.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&harness.root);
}

#[test]
fn readonly_dump_mount_is_visible_read_only_and_collisions_fail_closed() {
    let harness = Harness::new("oci-readonly-dump");
    let seed_dumps = harness.root.join("seed-dumps");
    fs::create_dir_all(&seed_dumps).expect("seed dumps");
    let dump_name = "seed.pgdump";
    let dump_host = seed_dumps.join(dump_name);
    fs::write(&dump_host, b"").expect("placeholder dump");

    let world = harness
        .engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            rootfs: harness.rootfs.clone(),
            snapshot: harness.snapshot.clone(),
            mounts: vec![
                WorldMount {
                    host_path: harness.repo.clone(),
                    guest_path: harness.repo.clone(),
                    mode: MountMode::ReadWrite,
                },
                WorldMount {
                    host_path: seed_dumps.clone(),
                    guest_path: seed_dumps.clone(),
                    mode: MountMode::ReadOnly,
                },
            ],
            base_env: world_env(&harness.home),
            local_ingress: None,
        })
        .expect("clone world with seed-dump mount");
    harness.init_postgres(&world);
    let postgres = harness.start_postgres(&world);
    harness.wait_postgres_ready(&postgres);
    assert_eq!(
        psql(&harness, &postgres, "create table seed(v text)").exit_code,
        0
    );
    assert_eq!(
        psql(&harness, &postgres, "insert into seed values ('restored')").exit_code,
        0
    );

    let dump = harness
        .engine
        .exec_in_service(
            &postgres,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "pg_dump -h 127.0.0.1 -U postgres -d postgres -Fc | base64 -w0".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("pg_dump inside postgres container");
    assert_eq!(dump.exit_code, 0, "pg_dump stderr: {}", dump.stderr);
    let bytes = support::decode_base64(&dump.stdout);
    assert!(
        bytes.len() > 100,
        "pg_dump must produce a real archive (stdout {} bytes, starts {:?}, stderr {})",
        dump.stdout.len(),
        dump.stdout.chars().take(80).collect::<String>(),
        dump.stderr
    );
    fs::write(&dump_host, &bytes).expect("write verified dump");

    let client = harness
        .engine
        .spawn_service(
            &world,
            &harness.oci_service(
                "pg-client",
                BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                &["sleep", "infinity"],
                env_with_path(PG_PATH),
                Vec::new(),
                vec![WorldMount {
                    host_path: dump_host.clone(),
                    guest_path: PathBuf::from("/dumps").join(dump_name),
                    mode: MountMode::ReadOnly,
                }],
                None,
            ),
        )
        .expect("spawn read-only client container");
    harness.wait_exec_ready(&client);

    let visible = harness
        .engine
        .exec_in_service(
            &client,
            &[
                "ls".to_owned(),
                "-l".to_owned(),
                format!("/dumps/{dump_name}"),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("ls readonly dump");
    assert_eq!(visible.exit_code, 0, "stderr: {}", visible.stderr);
    assert!(visible.stdout.contains(dump_name));

    let write = harness
        .engine
        .exec_in_service(
            &client,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo nope >> /dumps/{dump_name}"),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("write attempt to readonly dump");
    assert_ne!(write.exit_code, 0, "a read-only mount must reject writes");
    assert!(
        write.stderr.to_lowercase().contains("read-only")
            || write.stderr.to_lowercase().contains("readonly"),
        "expected EROFS, got stderr: {}",
        write.stderr
    );

    assert_eq!(
        psql(&harness, &postgres, "create database restored").exit_code,
        0
    );
    let restore = harness
        .engine
        .exec_in_service(
            &client,
            &[
                "pg_restore".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-d".to_owned(),
                "restored".to_owned(),
                format!("/dumps/{dump_name}"),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("pg_restore from the read-only dump");
    assert_eq!(
        restore.exit_code, 0,
        "pg_restore stderr: {}",
        restore.stderr
    );
    let restored = harness
        .engine
        .exec_in_service(
            &client,
            &[
                "psql".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-d".to_owned(),
                "restored".to_owned(),
                "-Atc".to_owned(),
                "select v from seed".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_| {},
        )
        .expect("query restored database");
    assert_eq!(restored.stdout.trim(), "restored");

    // A read-only mount that shadows a state mount is refused before any container exists.
    let err = harness
        .engine
        .spawn_service(
            &world,
            &harness.oci_service(
                "pg-collision",
                BackingServiceKey::Postgres {
                    version: "18".to_owned(),
                },
                &["sleep", "infinity"],
                env_with_path(PG_PATH),
                Harness::pg_state(),
                vec![WorldMount {
                    host_path: dump_host,
                    guest_path: PathBuf::from(PG_DATA),
                    mode: MountMode::ReadOnly,
                }],
                None,
            ),
        )
        .expect_err("colliding read-only/state mount must fail closed");
    assert!(
        matches!(err, EngineError::InvalidWorldSpec { .. }),
        "expected InvalidWorldSpec, got {err:?}"
    );
    assert!(
        format!("{err}").contains("collides"),
        "diagnostic must name the collision: {err}"
    );

    harness.engine.stop_service(&client).expect("stop client");
    harness.engine.destroy_world(world).expect("destroy world");
    let _ = fs::remove_dir_all(&harness.root);
}

/// The test binary must fail loudly (never skip) when its real inputs are missing.
#[test]
fn docker_and_pinned_crun_are_real_inputs() {
    assert!(support::pinned_crun().is_file(), "pinned crun must exist");
    let status = std::process::Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .status()
        .expect("docker must be installed for the OCI runtime test");
    assert!(status.success(), "the Docker daemon must be reachable");
}
