//! Protected lab journey for the single per-user daemon: real socket, real processes, zero mocks.
//!
//! `XDG_RUNTIME_DIR` and `XDG_STATE_HOME` live under `/var/tmp` (disk, never RAM `/tmp`), the
//! daemon is a real `slipfork __daemon` child, every reply is read from the real
//! `<runtime_dir>/daemon.sock`, and the world control plane is a real `ReferenceWorldControl`
//! unix-socket server speaking the canonical `slipfork_world::control` frames. A world is never
//! booted here: the control plane, the registry, `ps`, and child reaping are proven without Btrfs.

#[path = "support/reference_control.rs"]
mod reference_control;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reference_control::ReferenceWorldControl;
use slipfork_world::{
    ForkLedger, ForkName, ForkRequest, LiveFork, PodStatus, RootRequest, StatePaths, WorldName,
};

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-daemon-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create /var/tmp scratch directory");
    root
}

/// The canonical `Lease` JSON: a zero head is the lowercase hex digest, never a byte array.
fn seed_lease(world: &str, origin: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "world": world,
        "origin": origin,
        "world_epoch": "",
        "spec": "",
        "head": "0".repeat(64),
        "seeds": [],
        "services": {},
        "effects": {},
    }))
    .expect("encode the lease")
}

fn write_lease(store: &Path, world: &str, origin: &str) {
    let dir = store.join("worlds").join(world);
    fs::create_dir_all(&dir).expect("create the world data directory");
    fs::write(
        dir.join(slipfork_world::LEASE_FILE),
        seed_lease(world, origin),
    )
    .expect("write the world lease");
}

fn daemon_command(runtime: &Path, state: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("__daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
        .env("XDG_DATA_HOME", state.join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(state.join("slipfork/daemon.stderr")).expect("daemon stderr log"),
        ))
        .spawn()
        .expect("the slipfork binary must start the daemon")
}

fn wait_for_socket(socket: &Path, log: &Path, daemon: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            return;
        }
        if let Some(status) = daemon.try_wait().expect("poll daemon") {
            let detail = fs::read_to_string(log).unwrap_or_default();
            panic!("the daemon exited with {status} before binding: {detail}");
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("the daemon never bound `{}`", socket.display());
}

struct DaemonGuard {
    pid: u32,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Ok(pid) = i32::try_from(self.pid) {
            // SAFETY: `kill(2)` with a recorded pid never dereferences memory.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// Every `__daemon` whose environment names this test's runtime directory.
fn daemons_for(runtime: &Path) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let scope = runtime.as_os_str().as_bytes();
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").expect("read /proc") {
        let entry = entry.expect("read a /proc entry");
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if environ.windows(scope.len()).any(|window| window == scope) {
            found.push(pid);
        }
    }
    found
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect the daemon socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("ten-second reply timeout");
        let reader = BufReader::new(stream.try_clone().expect("clone the daemon stream"));
        Self { stream, reader }
    }

    fn call(&mut self, id: &str, method: &str, params: &str) -> serde_json::Value {
        let frame =
            format!("{{\"v\":1,\"id\":\"{id}\",\"method\":\"{method}\",\"params\":{params}}}\n");
        self.stream
            .write_all(frame.as_bytes())
            .expect("write the daemon request");
        self.stream.flush().expect("flush the daemon request");
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .expect("read the daemon reply");
        assert!(read > 0, "the daemon closed before replying to `{method}`");
        serde_json::from_str(line.trim()).expect("the daemon reply is JSON")
    }
}

fn processes_mentioning(needle: &[u8]) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").expect("read /proc") {
        let entry = entry.expect("read a /proc entry");
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if cmdline.windows(needle.len()).any(|window| window == needle) {
            found.push(pid);
        }
    }
    found
}

fn process_group(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_whitespace().nth(2))
        .and_then(|pgrp| pgrp.parse().ok())
}

/// Every world row and its nested forks, depth first.
fn flattened(worlds: &[serde_json::Value], out: &mut Vec<serde_json::Value>) {
    for world in worlds {
        out.push(world.clone());
        if let Some(forks) = world["forks"].as_array() {
            flattened(forks, out);
        }
    }
}

#[test]
fn daemon_control_plane_and_status_tree_lifecycle() {
    let runtime = scratch("rt");
    let state = scratch("state");
    let socket = runtime.join("daemon.sock");
    let state_root = state.join("slipfork");
    fs::create_dir_all(&state_root).expect("create the state directory");

    let project = state_root.join("demo-project");
    fs::create_dir_all(&project).expect("create the project directory");
    fs::write(
        project.join("Forkfile"),
        "# forkfile: 0.1\n\
         postgres pg {\n  version 18\n}\n\
         s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
         image app backend {\n\
         \x20 service api {\n    port 8000\n    route /api\n  }\n}\n",
    )
    .expect("write the Forkfile");

    // A retained live world: a real bound control socket plus a real, live supervisor pid.
    let live_pods = state_root.join("pods/kept");
    let control = ReferenceWorldControl::bind(
        &live_pods,
        vec![
            PodStatus {
                id: "api".to_owned(),
                pid: 4101,
                running: true,
                exit_code: None,
            },
            PodStatus {
                id: "pg".to_owned(),
                pid: 4102,
                running: true,
                exit_code: None,
            },
            PodStatus {
                id: "uploads".to_owned(),
                pid: 4103,
                running: true,
                exit_code: None,
            },
        ],
    );
    let mut supervisor = Command::new("sleep")
        .arg("120")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn the world supervisor");

    // The store root the daemon derives for this `/`-mounted project, seeded with a real lease so
    // convergence keeps the surviving `live` row instead of reaping it.
    let store = state.join("data/slipfork/store");
    write_lease(&store, "kept", "kept");

    let ledger = ForkLedger::open_writer(
        StatePaths::new(state_root.clone()),
        fs::File::create(state_root.join("seed.lock")).expect("create the seed lock"),
    )
    .expect("open the seed ledger writer");
    let kept = ledger
        .register_root(RootRequest {
            name: WorldName::parse("kept").expect("valid world name"),
            checkout_path: project.clone(),
            store_root: store.clone(),
            now_ms: 1_699_999_999_000,
        })
        .expect("register the retained root");
    ledger
        .publish_live(
            kept.id,
            LiveFork {
                supervisor_pid: i64::from(supervisor.id()),
                pods_dir: live_pods.clone(),
                started_at_ms: 1_700_000_000_000,
            },
            1_700_000_000_000,
        )
        .expect("publish the retained world live");

    // Two real untracked forks: their lease `origin` names the parent convergence adopts under.
    for (name, origin) in [("bonjour", "kept"), ("salut", "bonjour--kept")] {
        let checkout = project.join(".forks").join(name);
        fs::create_dir_all(&checkout).expect("create the fork checkout");
        fs::copy(project.join("Forkfile"), checkout.join("Forkfile"))
            .expect("copy the Forkfile into the fork");
        write_lease(&store, &format!("{name}--kept"), origin);
    }

    // A half-created fork: a `creating` row (parented under bonjour) with a partial checkout that
    // convergence must roll back, not adopt.
    let temporary_parent = ledger
        .begin_fork(ForkRequest {
            root_id: kept.id,
            parent_id: kept.id,
            name: ForkName::parse("bonjour").expect("valid fork name"),
            runtime_dir: runtime.clone(),
            branch: String::new(),
            now_ms: 1_700_000_000_000,
        })
        .expect("insert the temporary parent row");
    ledger
        .begin_fork(ForkRequest {
            root_id: kept.id,
            parent_id: temporary_parent.world_id,
            name: ForkName::parse("merci").expect("valid fork name"),
            runtime_dir: runtime.clone(),
            branch: String::new(),
            now_ms: 1_700_000_000_000,
        })
        .expect("insert the half-created fork");
    ledger
        .delete_world_row(temporary_parent.world_id)
        .expect("drop the temporary parent row");
    let merci = project.join(".forks/merci");
    fs::create_dir_all(&merci).expect("create the partial checkout");
    fs::write(merci.join(".partial-x"), "stage").expect("mark the partial checkout");

    // An orphan world whose pods exist in both the tmpfs runtime and the store.
    fs::create_dir_all(runtime.join("slipfork/pods/gone--kept/api"))
        .expect("create the orphan runtime pod");
    fs::create_dir_all(store.join("pods/gone--kept")).expect("create the orphan store pod");

    drop(ledger);
    // The pre-`1.0` registry: the daemon's writer must unlink it on open.
    fs::write(state_root.join("worlds.json"), "[]").expect("seed the legacy registry");

    let mut daemon = daemon_command(&runtime, &state);
    let guard = DaemonGuard { pid: daemon.id() };
    wait_for_socket(&socket, &state_root.join("daemon.stderr"), &mut daemon);
    let daemon_pid = {
        let mut client = Client::connect(&socket);
        let ping = client.call("ping-1", "ping", "{}");
        ping["ok"]["pid"]
            .as_u64()
            .expect("ping carries the daemon pid") as u32
    };
    assert_eq!(daemon_pid, daemon.id(), "ping must report its own pid");

    let mode = fs::metadata(&socket)
        .expect("stat the daemon socket")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon.sock must be mode 0600");

    let mut client = Client::connect(&socket);
    let ps = client.call("ps-1", "ps", "{}");
    assert_eq!(ps["v"], 1);
    let worlds = ps["ok"]["worlds"].as_array().expect("worlds array");
    let mut all = Vec::new();
    flattened(worlds, &mut all);
    assert!(
        all.iter()
            .all(|world| !world.to_string().contains("checkout missing")),
        "no adopted row may render a missing checkout: {all:?}"
    );
    let kept = worlds
        .iter()
        .find(|world| world["world"] == "kept")
        .expect("the retained root survives the daemon restart");
    assert_eq!(kept["url"], "http://kept.localhost");
    assert_eq!(kept["path"], project.to_string_lossy().as_ref());
    assert_eq!(kept["started_at_ms"], 1_700_000_000_000_u64);
    assert_eq!(kept["state"], "live", "the adopted world must stay live");
    assert_eq!(kept["supervisor_pid"], supervisor.id());
    let forks = kept["forks"].as_array().expect("forks array");
    let bonjour = forks
        .iter()
        .find(|fork| fork["world"] == "bonjour--kept")
        .unwrap_or_else(|| panic!("missing adopted fork `bonjour`: {forks:?}"));
    assert_eq!(bonjour["lineage"], serde_json::json!(["kept"]));
    let nested = bonjour["forks"].as_array().expect("nested forks array");
    let salut = nested
        .iter()
        .find(|fork| fork["world"] == "salut--kept")
        .unwrap_or_else(|| panic!("missing adopted fork `salut`: {nested:?}"));
    assert_eq!(salut["lineage"], serde_json::json!(["bonjour", "kept"]));
    assert!(
        all.iter().all(|world| world["world"] != "merci--kept"),
        "a `creating` row must be rolled back, never listed: {all:?}"
    );
    let services = kept["services"].as_array().expect("services array");
    assert_eq!(
        services.len(),
        3,
        "all three services survive: {services:?}"
    );
    assert!(services.iter().all(|service| service["running"] == true));
    let by_id = |id: &str| {
        services
            .iter()
            .find(|service| service["id"] == id)
            .unwrap_or_else(|| panic!("missing service `{id}`: {services:?}"))
    };
    assert_eq!(by_id("api")["kind"], "service");
    assert_eq!(by_id("api")["running"], true);
    assert_eq!(by_id("api")["port"], 8000);
    assert_eq!(by_id("api")["routes"][0]["prefix"], "/api");
    assert_eq!(by_id("api")["image"], "app");
    assert_eq!(by_id("pg")["kind"], "postgres");
    assert_eq!(by_id("pg")["version"], "18");
    assert_eq!(by_id("uploads")["kind"], "s3");
    assert_eq!(by_id("uploads")["cdn"], "assets-cdn");

    // The real ledger now owns exactly the three survivors, and the legacy registry is gone.
    let reader = ForkLedger::open_reader(state_root.clone()).expect("open the ledger reader");
    let rows = reader.list().expect("list the adopted ledger");
    assert_eq!(
        rows.worlds
            .iter()
            .filter(|row| row.world_name == "kept")
            .count(),
        1,
        "exactly one `kept` world row: {rows:?}"
    );
    assert_eq!(
        rows.worlds.len(),
        3,
        "kept plus the two adopted forks: {rows:?}"
    );
    assert!(
        rows.worlds
            .iter()
            .all(|row| row.world_name != "merci--kept"),
        "the half-created fork row must be deleted: {rows:?}"
    );
    assert!(
        !state_root.join("worlds.json").exists(),
        "the daemon must unlink the legacy registry"
    );
    assert!(
        !project.join(".forks/merci").exists(),
        "the partial fork checkout must be rolled back"
    );
    assert!(
        !runtime.join("slipfork/pods/gone--kept").exists(),
        "the orphan world's pod directory must be swept"
    );
    assert!(
        !store.join("pods/gone--kept").exists(),
        "the store pod sweep must run"
    );

    // The shipped binary renders the same world through the read-only client.
    let status = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["status", "--json"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("status must run");
    assert!(status.status.success(), "{status:?}");
    assert!(!String::from_utf8_lossy(&status.stdout).contains('\u{1b}'));
    let parsed: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status --json parses");
    assert_eq!(parsed["worlds"][0]["world"], "kept");

    let scoped = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["ps", "--json"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("ps must run");
    assert!(scoped.status.success(), "{scoped:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&scoped.stdout).expect("ps --json");
    assert_eq!(parsed["worlds"].as_array().map(Vec::len), Some(1));

    // Streaming verbs relay the world's typed frames, never re-rendered text.
    let logs = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "--json", "api"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("logs must run");
    assert!(logs.status.success(), "{logs:?}");
    let logs_text = String::from_utf8_lossy(&logs.stdout);
    assert!(!logs_text.contains('\u{1b}'));
    let mut frames: Vec<serde_json::Value> = logs_text
        .lines()
        .map(|line| serde_json::from_str(line).expect("each logs line is JSON"))
        .collect();
    let exit = frames.pop().expect("logs ends with an exit frame");
    assert_eq!(exit["exit"], 0, "{exit:?}");
    assert!(
        frames
            .iter()
            .any(|frame| frame["kind"] == "divider" && frame["pod"] == "api"),
        "logs must carry the divider: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["kind"] == "line" && frame["line"] == "hello from the pod"),
        "logs must carry the captured line: {frames:?}"
    );
    assert!(
        frames.iter().all(|frame| frame["stream"].is_string()),
        "every frame names its stream: {frames:?}"
    );

    let exec = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["exec", "api", "--", "echo", "hi"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("exec must run");
    assert_eq!(
        exec.status.code(),
        Some(7),
        "exec must forward the world's exit code: {exec:?}"
    );
    assert!(
        String::from_utf8_lossy(&exec.stdout).contains("exec-ok"),
        "exec must stream the child's stdout: {exec:?}"
    );

    // Duplex stdin: piped bytes reach the world and are echoed back.
    let mut piped = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["exec", "api", "--", "cat"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("piped exec must start");
    piped
        .stdin
        .take()
        .expect("exec stdin")
        .write_all(b"ping\n")
        .expect("write exec stdin");
    let echoed = piped.wait_with_output().expect("piped exec must finish");
    assert_eq!(
        echoed.status.code(),
        Some(7),
        "piped exec must forward the exit code: {echoed:?}"
    );
    let echoed_text = String::from_utf8_lossy(&echoed.stdout);
    assert!(echoed_text.contains("ping"), "{echoed_text:?}");

    // A followed stream exits 0 on SIGINT and the daemon observes the closed client.
    let follower = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "-f", "api"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("logs -f must start");
    thread::sleep(Duration::from_millis(400));
    // SAFETY: `kill(2)` targets the child we spawned.
    unsafe {
        libc::kill(follower.id() as i32, libc::SIGINT);
    }
    let followed = follower.wait_with_output().expect("logs -f must exit");
    assert_eq!(
        followed.status.code(),
        Some(0),
        "Ctrl-C on `logs -f` must exit 0: {followed:?}"
    );
    let daemon_log = state_root.join("daemon.stderr");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut closed = false;
    while Instant::now() < deadline {
        if fs::read_to_string(&daemon_log)
            .unwrap_or_default()
            .contains("{\"closed\":true}")
        {
            closed = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        closed,
        "the daemon must observe the follower close: {}",
        fs::read_to_string(&daemon_log).unwrap_or_default()
    );

    let human = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "api"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("human logs must run");
    assert!(human.status.success(), "{human:?}");
    let human_text = String::from_utf8_lossy(&human.stdout);
    assert!(!human_text.contains('\u{1b}'), "{human_text:?}");
    assert!(human_text.contains("api │"), "{human_text:?}");
    assert!(human_text.contains("── api started ──"), "{human_text:?}");

    // Bare `logs` merges application services only; explicit resource names work; `-a` includes all.
    let default_logs = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("logs")
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("bare logs must run");
    assert!(default_logs.status.success(), "{default_logs:?}");
    let default_text = String::from_utf8_lossy(&default_logs.stdout);
    assert!(!default_text.contains('\u{1b}'), "{default_text:?}");
    assert!(
        default_text.contains("api │ hello from the pod"),
        "{default_text:?}"
    );
    assert!(
        !default_text.contains("pg") && !default_text.contains("uploads"),
        "bare `logs` must exclude backing resources: {default_text:?}"
    );

    let pg_logs = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "pg"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("explicit resource logs must run");
    assert!(pg_logs.status.success(), "{pg_logs:?}");
    let pg_text = String::from_utf8_lossy(&pg_logs.stdout);
    assert!(!pg_text.contains('\u{1b}'), "{pg_text:?}");
    assert!(
        pg_text.contains("pg │ hello from the pod"),
        "explicit `logs pg` must stream that resource: {pg_text:?}"
    );

    // A native S3 bucket has no pod, so `logs uploads` must fail with the one-line remedy.
    let bucket_logs = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "uploads"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("explicit bucket logs must run");
    assert_eq!(bucket_logs.status.code(), Some(1), "{bucket_logs:?}");
    let bucket_err = String::from_utf8_lossy(&bucket_logs.stderr);
    assert!(!bucket_err.contains('\u{1b}'), "{bucket_err:?}");
    assert!(
        bucket_err.contains("✗ native S3 bucket 'uploads' has no pod logs"),
        "{bucket_err:?}"
    );
    assert!(
        bucket_err.contains("next: slipfork shell api"),
        "{bucket_err:?}"
    );
    assert_eq!(
        bucket_err.matches("next:").count(),
        1,
        "the bucket refusal carries exactly one remedy: {bucket_err:?}"
    );

    let all_logs = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["logs", "-a"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .output()
        .expect("logs -a must run");
    assert!(all_logs.status.success(), "{all_logs:?}");
    let all_text = String::from_utf8_lossy(&all_logs.stdout);
    assert!(!all_text.contains('\u{1b}'), "{all_text:?}");
    assert!(
        all_text.contains("api │ hello from the pod"),
        "{all_text:?}"
    );
    assert!(
        all_text.contains("pg  │ hello from the pod"),
        "{all_text:?}"
    );
    assert!(
        !all_text.contains("uploads"),
        "native S3 has no pod logs, so `logs -a` never targets a bucket: {all_text:?}"
    );

    // A target-less `exec` refuses every running candidate and exits 1 with one remedy.
    let ambiguous = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["exec", "--", "true"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("a target-less exec must run");
    assert_eq!(ambiguous.status.code(), Some(1), "{ambiguous:?}");
    let ambiguous_err = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        ambiguous_err.contains("error: 2 candidates: api, pg"),
        "{ambiguous_err:?}"
    );
    assert!(
        ambiguous_err.contains("next: pass one of the ids above"),
        "{ambiguous_err:?}"
    );

    let missing = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("shell")
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("a target-less shell must run");
    assert_eq!(missing.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("required arguments"),
        "a missing target must be a clap usage error: {stderr}"
    );
    assert_eq!(
        stderr.matches("next:").count(),
        1,
        "a usage error carries exactly one remedy: {stderr}"
    );

    // Two concurrent clients get coherent ping/ps replies.
    let mut handles = Vec::new();
    for index in 0..2 {
        let socket = socket.clone();
        handles.push(thread::spawn(move || {
            let mut client = Client::connect(&socket);
            let ping = client.call(&format!("ping-{index}"), "ping", "{}");
            assert_eq!(ping["ok"]["pong"], true);
            assert_eq!(ping["id"], format!("ping-{index}"));
            let ps = client.call(&format!("ps-{index}"), "ps", "{}");
            assert_eq!(ps["id"], format!("ps-{index}"));
        }));
    }
    for handle in handles {
        handle.join().expect("concurrent client completed");
    }

    let unknown = client.call("bad-1", "frobnicate", "{}");
    assert_eq!(unknown["error"]["code"], "unknown-method");
    let ping = client.call("ping-after", "ping", "{}");
    assert_eq!(ping["ok"]["pong"], true, "the connection must stay open");

    let _ = supervisor.kill();
    let _ = supervisor.wait();

    let shutdown = client.call("bye", "shutdown", "{}");
    assert_eq!(shutdown["ok"]["stopped"], true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if daemon.try_wait().expect("poll the daemon").is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        daemon.try_wait().expect("poll the daemon").is_some(),
        "the daemon must exit after `shutdown`"
    );
    assert!(
        !Path::new(&format!("/proc/{daemon_pid}")).exists(),
        "the daemon pid must be gone"
    );
    let members: Vec<u32> = fs::read_dir("/proc")
        .expect("read /proc")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| process_group(*pid) == Some(daemon_pid))
        .collect();
    assert!(
        members.is_empty(),
        "no process may remain in the daemon's group: {members:?}"
    );
    let needle = runtime.to_string_lossy().into_owned();
    thread::sleep(Duration::from_millis(200));
    assert!(
        processes_mentioning(needle.as_bytes()).is_empty(),
        "no daemon/supervisor may survive shutdown"
    );
    std::mem::forget(guard);
    assert!(
        daemons_for(&runtime).is_empty(),
        "every `__daemon` for this runtime must be reaped"
    );

    drop(control);
    let _ = fs::remove_dir_all(&runtime);
    let _ = fs::remove_dir_all(&state);
}
