//! Protected critical tests for `slipfork-engine` (`R10`, `R11`, `R12`).
//!
//! The reflink tests run against the real filesystem (`ioctl(FICLONE)` on XFS `reflink=1`) and
//! real unprivileged mount/net/pid namespaces — never a mocked CoW engine. The Incus adapter is
//! exercised against an in-tree Unix-socket protocol harness only (live `incusd` is gated on
//! `H-004`).

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use slipfork_engine::conformance::assert_world_engine_conformance;
use slipfork_engine::{
    EngineError, ExecEnv, ExecOutput, IncusEngine, InheritedVar, MountMode, ReflinkNetnsEngine,
    SnapshotId, SnapshotMode, StreamChunk, WorldEngine, WorldHandle, WorldMount, WorldSpec,
    current_uid_gid, probe_reflink_dir,
};
use slipfork_world::WorldTarget;

fn xfs_test_dir(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("slipfork-critical-tests")
        .join(format!("{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create xfs test dir");
    dir
}

fn snap(label: &str) -> SnapshotId {
    SnapshotId(label.to_owned())
}

/// World environment with the inherited allowlist and a world-scoped `HOME`.
fn world_env(home: &Path) -> ExecEnv {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(
        InheritedVar::Path,
        std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_owned()),
    );
    env
}

fn spec(
    target: WorldTarget,
    snapshot: &SnapshotId,
    home: &Path,
    mounts: Vec<WorldMount>,
) -> WorldSpec {
    WorldSpec {
        target,
        snapshot: snapshot.clone(),
        mounts,
        base_env: world_env(home),
        local_ingress_port: None,
    }
}

fn run(engine: &ReflinkNetnsEngine, world: &WorldHandle, cwd: &Path, script: &str) -> ExecOutput {
    let mut chunks: Vec<StreamChunk> = Vec::new();
    engine
        .exec_in_world(
            world,
            &["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd,
            &ExecEnv::default(),
            &mut |chunk| chunks.push(chunk),
        )
        .expect("exec inside world")
}

#[test]
fn r10_tmpfs_fails_closed_without_silent_byte_copy_fallback() {
    let tmp_dir =
        PathBuf::from("/tmp/opencode").join(format!("slipfork-tmpfs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    let err = probe_reflink_dir(&tmp_dir).expect_err("/tmp tmpfs must fail FICLONE closed");
    let _ = fs::remove_dir_all(&tmp_dir);
    assert!(matches!(err, EngineError::ReflinkUnsupported { .. }));
}

#[test]
fn r10_r11_reflink_netns_conformance_cow_isolation_uid_and_parallel_same_port() {
    let root = xfs_test_dir("reflink-netns");
    let (uid, gid) = current_uid_gid();
    let engine = ReflinkNetnsEngine::new(root.join("engine"), uid, gid);

    // 1. Shared WorldEngine contract (fail-closed errors, redaction, opaque handles, seal-once).
    assert_world_engine_conformance(&engine, &root);

    // 2. Real CoW snapshot + two simultaneous clones of the same snapshot.
    let src_state = root.join("src-state");
    fs::create_dir_all(&src_state).expect("src_state");
    fs::write(src_state.join("seed.txt"), b"ancestor-row-1\n").expect("write seed");
    engine
        .create_snapshot(&src_state, &snap("seed-v1"), SnapshotMode::DiskCow)
        .expect("snapshot");

    let workspace_a = root.join("ws-a");
    let workspace_b = root.join("ws-b");
    fs::create_dir_all(&workspace_a).expect("ws-a");
    fs::create_dir_all(&workspace_b).expect("ws-b");
    let home_a = root.join("home-a");
    let home_b = root.join("home-b");
    let mount_at = |path: &Path| WorldMount {
        host_path: path.to_path_buf(),
        guest_path: path.to_path_buf(),
        mode: MountMode::ReadWrite,
    };
    let world_a = engine
        .clone_world(&spec(
            WorldTarget::Canonical,
            &snap("seed-v1"),
            &home_a,
            vec![mount_at(&workspace_a)],
        ))
        .expect("clone world a");
    let world_b = engine
        .clone_world(&spec(
            WorldTarget::UserFork("b".to_owned()),
            &snap("seed-v1"),
            &home_b,
            vec![mount_at(&workspace_b)],
        ))
        .expect("clone world b");

    // 3. World A's writes (state + shared workspace) never leak into world B; files stay host-owned.
    let mut streamed: Vec<StreamChunk> = Vec::new();
    let out = engine
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
    assert_eq!(lines.next(), Some(home_a.display().to_string().as_str()));
    assert_eq!(lines.next(), Some(uid.to_string().as_str()));
    assert_eq!(lines.next(), Some(gid.to_string().as_str()));
    let generated = fs::metadata(workspace_a.join("generated.txt")).expect("generated.txt");
    assert_eq!(generated.uid(), uid, "files must stay host-owned");
    assert_eq!(generated.gid(), gid, "files must stay host-owned");
    let out = run(
        &engine,
        &world_b,
        &workspace_b,
        "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(
        out.stdout, "ancestor-row-1\n",
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
        let engine = engine.clone();
        thread::spawn(move || {
            engine
                .exec_in_world(
                    &world,
                    &[
                        "uv".to_owned(),
                        "run".to_owned(),
                        "--no-project".to_owned(),
                        "python".to_owned(),
                        "-c".to_owned(),
                        bind_py.to_owned(),
                    ],
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

    // 5. Stateful RAM snapshots have no capability flag: the error type is the contract.
    let err = engine
        .create_snapshot(&src_state, &snap("ram"), SnapshotMode::StatefulRam)
        .expect_err("reflink engine cannot capture RAM state");
    assert!(matches!(
        err,
        EngineError::StatefulRamUnsupported {
            engine: "reflink-netns"
        }
    ));

    engine.destroy_world(world_a).expect("destroy world a");
    engine.destroy_world(world_b).expect("destroy world b");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn multi_mount_repo_rw_and_readonly_artifact_volume_are_real_bind_mounts() {
    let root = xfs_test_dir("reflink-netns-mounts");
    let (uid, gid) = current_uid_gid();
    let engine = ReflinkNetnsEngine::new(root.join("engine"), uid, gid);

    let src_state = root.join("state-src");
    fs::create_dir_all(&src_state).expect("state-src");
    fs::write(src_state.join("seed.txt"), b"ancestor\n").expect("seed");
    engine
        .create_snapshot(&src_state, &snap("base-mounted"), SnapshotMode::DiskCow)
        .expect("base snapshot");

    let repo = root.join("repo");
    let artifacts = root.join("artifacts");
    fs::create_dir_all(&repo).expect("repo");
    fs::create_dir_all(&artifacts).expect("artifacts");
    fs::write(artifacts.join("rows.txt"), b"artifact-row\n").expect("artifact row");

    let mounts = vec![
        WorldMount {
            host_path: repo.clone(),
            guest_path: repo.clone(),
            mode: MountMode::ReadWrite,
        },
        WorldMount {
            host_path: artifacts.clone(),
            guest_path: artifacts.clone(),
            mode: MountMode::ReadOnly,
        },
    ];
    let world = engine
        .clone_world(&spec(
            WorldTarget::UserFork("mounts".to_owned()),
            &snap("base-mounted"),
            &root.join("home"),
            mounts,
        ))
        .expect("clone multi-mount world");

    let out = run(
        &engine,
        &world,
        &repo,
        &format!("cat {}/rows.txt", artifacts.display()),
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("artifact-row"),
        "stdout: {}",
        out.stdout
    );

    let out = run(
        &engine,
        &world,
        &repo,
        &format!(
            "cat {}/rows.txt && echo world-write > from-world.txt",
            artifacts.display()
        ),
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("artifact-row"));
    let written = fs::metadata(repo.join("from-world.txt")).expect("RW mount write");
    assert_eq!(written.uid(), uid);
    assert_eq!(written.gid(), gid);

    // The read-only artifact volume really is read-only inside the world.
    let out = run(
        &engine,
        &world,
        &repo,
        &format!("echo nope > {}/rows.txt", artifacts.display()),
    );
    assert_ne!(out.exit_code, 0, "RO writes must fail closed");
    assert!(
        fs::read_to_string(artifacts.join("rows.txt"))
            .expect("artifact still intact")
            .contains("artifact-row")
    );

    // Nested read-write mounts are refused before any world state is materialized.
    let nested = spec(
        WorldTarget::UserFork("nested".to_owned()),
        &snap("base-mounted"),
        &root.join("home"),
        vec![
            WorldMount {
                host_path: repo.clone(),
                guest_path: repo.clone(),
                mode: MountMode::ReadWrite,
            },
            WorldMount {
                host_path: repo.join("artifacts"),
                guest_path: repo.join("artifacts"),
                mode: MountMode::ReadWrite,
            },
        ],
    );
    let err = engine
        .clone_world(&nested)
        .expect_err("nested RW mounts must fail closed");
    assert!(matches!(err, EngineError::OverlappingMounts { .. }));

    engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn freeze_lineage_seals_once_and_sibling_clones_stay_isolated() {
    let root = xfs_test_dir("reflink-netns-freeze");
    let (uid, gid) = current_uid_gid();
    let engine = ReflinkNetnsEngine::new(root.join("engine"), uid, gid);

    let src_state = root.join("state-src");
    fs::create_dir_all(&src_state).expect("state-src");
    fs::write(src_state.join("seed.txt"), b"ancestor\n").expect("seed");
    engine
        .create_snapshot(&src_state, &snap("seed-base"), SnapshotMode::DiskCow)
        .expect("base snapshot");

    let home = root.join("home");
    let origin = engine
        .clone_world(&spec(
            WorldTarget::Canonical,
            &snap("seed-base"),
            &home,
            vec![],
        ))
        .expect("origin world");
    let out = run(
        &engine,
        &origin,
        &root,
        "echo migrated >> \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);

    engine
        .freeze_world(&origin, &snap("seed-frozen"), SnapshotMode::DiskCow)
        .expect("seal origin world");
    let err = engine
        .freeze_world(&origin, &snap("seed-frozen-again"), SnapshotMode::DiskCow)
        .expect_err("an already-frozen world must fail closed");
    assert!(matches!(err, EngineError::WorldNotFreezable { .. }));

    let frozen = snap("seed-frozen");
    let mut sibling_spec = spec(
        WorldTarget::UserFork("sib".to_owned()),
        &frozen,
        &home,
        vec![],
    );
    let sibling_a = engine.clone_world(&sibling_spec).expect("sibling a");
    sibling_spec.target = WorldTarget::UserFork("sib2".to_owned());
    let sibling_b = engine.clone_world(&sibling_spec).expect("sibling b");

    let out = run(
        &engine,
        &sibling_a,
        &root,
        "echo sibling-mutation >> \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);

    let out = run(
        &engine,
        &sibling_b,
        &root,
        "cat \"$SLIPFORK_WORLD_STATE/seed.txt\"",
    );
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout, "ancestor\nmigrated\n",
        "a sibling clone of the frozen snapshot must never see another clone's writes"
    );

    // Freeze-after-destroy and double-destroy both fail closed.
    let destroyed = sibling_a.clone();
    engine.destroy_world(sibling_a).expect("destroy sibling a");
    let err = engine
        .freeze_world(&destroyed, &snap("never"), SnapshotMode::DiskCow)
        .expect_err("a destroyed world cannot be frozen");
    assert!(matches!(err, EngineError::WorldNotFreezable { .. }));
    let err = engine
        .destroy_world(destroyed)
        .expect_err("freeze-then-destroy double-free must fail closed");
    assert!(matches!(err, EngineError::WorldGone { .. }));

    engine.destroy_world(sibling_b).expect("destroy sibling b");
    engine.destroy_world(origin).expect("destroy origin");
    let _ = fs::remove_dir_all(&root);
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
    let full = spec(
        WorldTarget::Canonical,
        &snap("whatever"),
        Path::new("/w/home"),
        vec![],
    );
    let spec_debug = format!("{full:?}");
    assert!(!spec_debug.contains(SECRET));

    // Handles are opaque (stable Debug across clones, no filesystem/env payload) and the real
    // child-process boundary is the only place the effective `base ⊕ overlay` values materialize.
    let root = xfs_test_dir("reflink-netns-redact");
    let (uid, gid) = current_uid_gid();
    let engine = ReflinkNetnsEngine::new(root.join("engine"), uid, gid);
    let src_state = root.join("state-src");
    fs::create_dir_all(&src_state).expect("state-src");
    fs::write(src_state.join("seed.txt"), b"seed\n").expect("seed");
    engine
        .create_snapshot(&src_state, &snap("redact-base"), SnapshotMode::DiskCow)
        .expect("snapshot");

    let home = root.join("home");
    let mut base_env = ExecEnv::world_scoped(&home);
    base_env.inherit(
        InheritedVar::Path,
        std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_owned()),
    );
    base_env.set_secret("API_TOKEN", SECRET).expect("secret");
    base_env
        .set("APP_URL", "http://acme.localhost")
        .expect("plain");
    let world = engine
        .clone_world(&WorldSpec {
            target: WorldTarget::Canonical,
            snapshot: snap("redact-base"),
            mounts: vec![],
            base_env,
            local_ingress_port: None,
        })
        .expect("clone");
    let mut chunks = Vec::new();
    let out = engine
        .exec_in_world(
            &world,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "printf '%s|%s|%s' \"$API_TOKEN\" \"$APP_URL\" \"$HOME\"".to_owned(),
            ],
            &root,
            &overlay,
            &mut |chunk| chunks.push(chunk),
        )
        .expect("exec world");
    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout,
        format!(
            "{OVERLAY_SECRET}|http://overlay.localhost|{}",
            home.display()
        ),
        "the child sees base ⊕ overlay, with the overlay secret winning"
    );
    assert!(
        !out.stdout.contains(SECRET),
        "the overridden base secret must never reach the child"
    );

    let clone = world.clone();
    assert_eq!(format!("{world:?}"), format!("{clone:?}"));
    assert!(!format!("{world:?}").contains("redact-base"));
    assert!(!format!("{world:?}").contains(root.display().to_string().as_str()));
    assert!(!format!("{world:?}").contains(SECRET));
    engine.destroy_world(world).expect("destroy");
    let _ = fs::remove_dir_all(&root);
}

/// Minimal Incus `/1.0` Unix-socket protocol harness: records requests, answers like `incusd`.
struct FakeIncus {
    socket: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeIncus {
    fn start(socket: PathBuf) -> Self {
        let listener = UnixListener::bind(&socket).expect("bind fake incus socket");
        listener
            .set_nonblocking(true)
            .expect("non-blocking fake incus listener");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = read_http_request(&mut stream);
                            requests.lock().expect("requests").push(request.clone());
                            let response = incus_response(&request);
                            let _ = stream.write_all(response.as_bytes());
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            socket,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for FakeIncus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_http_request(stream: &mut UnixStream) -> String {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("read fake incus request");
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&raw);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let content_length = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if body.len() >= content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

fn incus_response(request: &str) -> String {
    let status_line = request.lines().next().unwrap_or_default();
    let (status, body) = if request.contains("definitely-missing") {
        (
            "404 Not Found",
            r#"{"error":"snapshot not found","error_code":404}"#,
        )
    } else if status_line.contains("/exec") {
        (
            "200 OK",
            r#"{"type":"sync","status":"Success","status_code":200,"metadata":{"metadata":{"return":0,"stdout":"world-ok\n","stderr":""}}}"#,
        )
    } else {
        (
            "200 OK",
            r#"{"type":"sync","status":"Success","status_code":200,"metadata":{}}"#,
        )
    };
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn r12_incus_engine_protocol_harness_matches_conformance_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("incus.socket");
    let missing = IncusEngine::new(
        dir.path().join("missing.socket"),
        "default",
        "acme-base",
        1000,
        1000,
    );
    let err = missing
        .clone_world(&spec(
            WorldTarget::Canonical,
            &snap("mig"),
            dir.path(),
            vec![],
        ))
        .expect_err("must fail closed when the incusd socket is absent");
    assert!(matches!(err, EngineError::DaemonUnavailable { .. }));

    let fake = FakeIncus::start(socket.clone());
    let incus = IncusEngine::new(&socket, "default", "acme-base", 1000, 1000);
    assert_world_engine_conformance(&incus, dir.path());

    let repo = dir.path().join("repo");
    let volumes = dir.path().join("volumes");
    fs::create_dir_all(&repo).expect("repo");
    fs::create_dir_all(&volumes).expect("volumes");
    let world = incus
        .clone_world(&spec(
            WorldTarget::UserFork("incus".to_owned()),
            &snap("seed-catalog"),
            &dir.path().join("home"),
            vec![
                WorldMount {
                    host_path: repo.clone(),
                    guest_path: repo.clone(),
                    mode: MountMode::ReadWrite,
                },
                WorldMount {
                    host_path: volumes.clone(),
                    guest_path: volumes.clone(),
                    mode: MountMode::ReadOnly,
                },
            ],
        ))
        .expect("incus clone");

    let mut overlay = ExecEnv::default();
    overlay
        .set_secret("DB_PASSWORD", "incus-secret")
        .expect("secret overlay");
    let mut chunks = Vec::new();
    let out = incus
        .exec_in_world(
            &world,
            &["cat".to_owned(), "rows.txt".to_owned()],
            &repo,
            &overlay,
            &mut |chunk| chunks.push(chunk),
        )
        .expect("incus exec");
    assert_eq!(out.stdout, "world-ok\n");
    assert!(!chunks.is_empty(), "incus exec forwards stdout as a chunk");

    let requests = fake.requests();
    let clone_request = requests
        .iter()
        .find(|request| request.contains(r#""name":"fork-incus-"#))
        .expect("instance clone request");
    assert!(clone_request.contains(r#""shift":"true""#));
    assert!(clone_request.contains(r#""readonly":"true""#));
    assert!(clone_request.contains(r#""type":"copy""#));
    let exec_request = requests
        .iter()
        .find(|request| request.contains("/exec"))
        .expect("exec request");
    assert!(exec_request.contains(r#""DB_PASSWORD":"incus-secret""#));
    assert!(exec_request.contains(&repo.display().to_string()));

    incus.destroy_world(world).expect("incus destroy");
}
