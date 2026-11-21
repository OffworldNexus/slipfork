//! Protected lab journey for the single per-user daemon: real socket, real processes, zero mocks.
//!
//! `XDG_RUNTIME_DIR` and `XDG_STATE_HOME` live under `/var/tmp` (disk, never RAM `/tmp`), the
//! daemon is a real `slipfork __daemon` child, and every reply is read from the real
//! `<runtime_dir>/daemon.sock`. A world is never booted here: the control plane, the registry, and
//! child reaping are proven without Btrfs.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::path::{Path, PathBuf};

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

fn daemon_command(runtime: &Path, state: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("__daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
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

/// A real `control.sock` stand-in for a live world; it answers `List` and `Shutdown`.
struct FakeControl {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeControl {
    fn bind(pods_dir: &Path) -> Self {
        fs::create_dir_all(pods_dir).expect("create fake pod root");
        let listener =
            UnixListener::bind(pods_dir.join("control.sock")).expect("bind control.sock");
        fs::set_permissions(
            pods_dir.join("control.sock"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("chmod control.sock");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking reply stream");
                        let mut line = String::new();
                        if BufReader::new(stream.try_clone().expect("clone"))
                            .read_line(&mut line)
                            .is_err()
                        {
                            continue;
                        }
                        let request: serde_json::Value =
                            serde_json::from_str(line.trim()).unwrap_or_default();
                        let id = request["id"].as_str().unwrap_or_default();
                        let method = request["method"].as_str().unwrap_or_default();
                        let reply = if method == "shutdown" {
                            serde_json::json!({"kind":"reply","v":1,"id":id,"ok":{"stopped":true}})
                        } else {
                            serde_json::json!({"kind":"reply","v":1,"id":id,"ok":{"pods":[]}})
                        };
                        let _ = stream.write_all(reply.to_string().as_bytes());
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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

#[test]
fn daemon_control_plane_lifecycle() {
    let runtime = scratch("rt");
    let state = scratch("state");
    let socket = runtime.join("daemon.sock");
    let state_root = state.join("slipfork");
    fs::create_dir_all(&state_root).expect("create the state directory");

    // A retained live world: a real bound control socket plus a real, live supervisor pid.
    let live_pods = state_root.join("pods/kept");
    let control = FakeControl::bind(&live_pods);
    let mut fake_supervisor = Command::new("sleep")
        .arg("120")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn the fake supervisor");
    let worlds_file = state_root.join("worlds.json");
    let retained = serde_json::json!([
        {
            "project": "/var/tmp/kept-project",
            "world": "kept",
            "pods_dir": live_pods,
            "supervisor_pid": fake_supervisor.id(),
        },
        {
            "project": "/var/tmp/forgotten-project",
            "world": "forgotten",
            "pods_dir": "/var/tmp/forgotten-pods",
            "supervisor_pid": 999_999_u32,
        }
    ]);
    fs::write(
        &worlds_file,
        serde_json::to_vec(&retained).expect("encode seed registry"),
    )
    .expect("seed the registry");

    let mut daemon = daemon_command(&runtime, &state);
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

    // (c) the live entry survives, the dead one is forgotten.
    let mut client = Client::connect(&socket);
    let ps = client.call("ps-1", "ps", "{}");
    assert_eq!(ps["v"], 1);
    let worlds = ps["ok"]["worlds"].as_array().expect("worlds array");
    assert_eq!(
        worlds.len(),
        1,
        "only the live retained world survives: {worlds:?}"
    );
    assert_eq!(worlds[0]["world"], "kept");
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(&worlds_file).expect("read the registry"))
            .expect("registry is JSON");
    assert_eq!(stored.as_array().map(Vec::len), Some(1));
    assert_eq!(stored[0]["world"], "kept");

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

    // An unknown method is a typed error and the same connection still answers.
    let unknown = client.call("bad-1", "frobnicate", "{}");
    assert_eq!(unknown["error"]["code"], "unknown-method");
    let ping = client.call("ping-after", "ping", "{}");
    assert_eq!(ping["ok"]["pong"], true, "the connection must stay open");

    // The kept world's fake supervisor is gone before shutdown, so teardown stays instant.
    let _ = fake_supervisor.kill();
    let _ = fake_supervisor.wait();

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

    drop(control);
    let _ = fs::remove_dir_all(&runtime);
    let _ = fs::remove_dir_all(&state);
}
