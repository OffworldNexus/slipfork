//! Two real binaries on one socket: a daemon started from one path must be refused by another.
//!
//! Every process here is real — a copied `slipfork` binary runs as the daemon and a second copy
//! runs as the client, plus a re-executed test binary that answers `ping` the pre-identity way.
//! No Btrfs, no pods, and no mock transport: only `ping`, `status`, and the typed `daemon-skew`
//! refusal that stops a stale daemon from serving new code — and that names its pid.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A disk scratch tree under `/var/tmp` (never RAM `/tmp`), removed with its daemons.
struct Scratch(PathBuf);

impl Scratch {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos();
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-skew-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create /var/tmp scratch directory");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn state(&self) -> PathBuf {
        let state = self.0.join("state");
        fs::create_dir_all(&state).expect("create the state directory");
        state
    }

    fn runtime(&self, name: &str) -> PathBuf {
        let runtime = self.0.join(name);
        fs::create_dir_all(&runtime).expect("create the runtime directory");
        // SAFETY: `chmod(2)` on a path this test just created.
        unsafe {
            libc::chmod(
                std::ffi::CString::new(runtime.as_os_str().as_encoded_bytes())
                    .expect("no NUL in the runtime path")
                    .as_ptr(),
                0o700,
            );
        }
        runtime
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Copies the shipped binary to `name`, so its canonical path differs from every other copy.
fn copy_binary(root: &Path, name: &str) -> PathBuf {
    let directory = root.join(name);
    fs::create_dir_all(&directory).expect("create the binary directory");
    let destination = directory.join(name);
    fs::copy(env!("CARGO_BIN_EXE_slipfork"), &destination).expect("copy the slipfork binary");
    fs::canonicalize(&destination).expect("canonicalize the copied binary")
}

fn daemon(exe: &Path, runtime: &Path, state: &Path) -> Child {
    Command::new(exe)
        .arg("__daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(state.join("daemon.stderr")).expect("daemon stderr log"),
        ))
        .spawn()
        .expect("the copied binary must start the daemon")
}

fn wait_for_socket(socket: &Path, log: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll the daemon") {
            let detail = fs::read_to_string(log).unwrap_or_default();
            panic!("the daemon exited with {status} before binding: {detail}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the daemon never bound `{}`", socket.display());
}

fn client(exe: &Path, runtime: &Path, state: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("run the client binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Reads one typed JSON refusal, proving the message and the remedy never duplicate.
fn refusal(output: &Output, verb: &str) -> serde_json::Value {
    let stderr = text(&output.stderr);
    assert!(!output.status.success(), "`{verb}` must refuse: {stderr}");
    assert!(
        stderr.is_empty(),
        "`{verb}` writes no prose to stderr: {stderr}"
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "`{verb}` emits one JSON document ({error}): {:?}",
            text(&output.stdout)
        )
    });
    let error = &value["error"];
    assert_eq!(error["code"], "daemon-skew", "{value}");
    let message = error["message"].as_str().expect("a message string");
    assert!(
        !message.contains("next:"),
        "`{verb}` embeds a remedy in its message: {message}"
    );
    let next = error["next"].as_str().expect("a next string");
    assert!(
        !next.contains("next:"),
        "`{verb}` embeds a remedy in its next: {next}"
    );
    assert_eq!(
        error.as_object().map_or(0, serde_json::Map::len),
        3,
        "`{verb}` carries one code, one message, one next: {value}"
    );
    value
}

/// The pid-bearing remedy: kill exactly this process, then let the CLI start its own daemon.
fn assert_stale_remedy(next: &str, pid: u32) {
    assert!(
        next.contains(&format!("kill {pid} (the stale daemon)")),
        "the remedy names pid {pid}: {next}"
    );
    assert!(next.contains("slipfork up"), "the remedy re-ups: {next}");
    assert!(
        !next.contains("systemctl"),
        "a live pid needs no unit command: {next}"
    );
}

/// The human stderr of a refusal: one remedy line, naming the stale pid, plain when piped.
fn assert_stale_stderr(stderr: &str, pid: u32) {
    assert!(
        !stderr.contains('\u{1b}'),
        "a piped client is plain: {stderr:?}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("next: "))
            .count(),
        1,
        "exactly one remedy line: {stderr}"
    );
    let next = stderr
        .lines()
        .find(|line| line.starts_with("next: "))
        .unwrap_or_else(|| panic!("no remedy line: {stderr}"));
    assert_stale_remedy(next, pid);
}

/// Kills every `__daemon` (and the re-executed old-style server) whose environment names one of
/// this test's scratch runtime directories.
struct Daemons {
    runtimes: Vec<PathBuf>,
}

impl Daemons {
    fn new(runtime: PathBuf) -> Self {
        Self {
            runtimes: vec![runtime],
        }
    }

    fn add(&mut self, runtime: PathBuf) {
        self.runtimes.push(runtime);
    }
}

impl Drop for Daemons {
    fn drop(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        for runtime in &self.runtimes {
            let scope = runtime.as_os_str().as_bytes();
            for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<i32>().ok())
                else {
                    continue;
                };
                let path = entry.path();
                let Ok(cmdline) = fs::read(path.join("cmdline")) else {
                    continue;
                };
                let is_daemon = {
                    let line = String::from_utf8_lossy(&cmdline);
                    line.contains("__daemon") || line.contains("old_style_daemon_server")
                };
                if !is_daemon {
                    continue;
                }
                let Ok(environ) = fs::read(path.join("environ")) else {
                    continue;
                };
                if environ.windows(scope.len()).any(|window| window == scope) {
                    // SAFETY: `kill(2)` with a live pid never dereferences memory.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

/// Server mode of the test binary: bind `$XDG_RUNTIME_DIR/daemon.sock` and answer `ping` with
/// no `version`/`exe`, exactly like a daemon that predates the identity handshake.
///
/// It returns immediately (green) unless `SLIPFORK_OLD_DAEMON` names a scratch runtime, so the
/// parent can re-exec `current_exe()` and get the real old wire shape around `ping` only.
#[test]
fn old_style_daemon_server() {
    if std::env::var_os("SLIPFORK_OLD_DAEMON").is_none() {
        return;
    }
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return;
    };
    let socket = runtime.join("daemon.sock");
    let _ = fs::remove_file(&socket);
    let listener =
        std::os::unix::net::UnixListener::bind(&socket).expect("bind the old-style daemon socket");
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .expect("chmod the old-style daemon socket");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 512];
        while !raw.contains(&b'\n') {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => raw.extend_from_slice(&buffer[..read]),
            }
        }
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = request["id"].as_str().unwrap_or_default();
        let reply = serde_json::json!({
            "v": 1,
            "id": id,
            "ok": { "pong": true, "pid": std::process::id(), "worlds": 0 },
        });
        let _ = stream.write_all(format!("{reply}\n").as_bytes());
        let _ = stream.flush();
    }
}

#[test]
fn a_stale_daemon_is_refused_and_an_auto_spawned_one_matches() {
    let scratch = Scratch::new("journey");
    let root = scratch.path();
    let state = scratch.state();
    let alpha = copy_binary(root, "slipfork-alpha");
    let beta = copy_binary(root, "slipfork-beta");
    let gamma = copy_binary(root, "slipfork-gamma");
    let delta = copy_binary(root, "slipfork-delta");

    // A daemon started from one path makes the other path refuse it with exactly one remedy.
    let runtime = scratch.runtime("runtime");
    let mut daemons = Daemons::new(runtime.clone());
    let socket = runtime.join("daemon.sock");
    let mut started = daemon(&alpha, &runtime, &state);
    wait_for_socket(&socket, &state.join("daemon.stderr"), &mut started);
    let started_pid = started.id();

    let same = client(&alpha, &runtime, &state, root, &["status", "--json"]);
    assert!(
        same.status.success(),
        "the spawning binary must talk to its own daemon: {}{}",
        text(&same.stdout),
        text(&same.stderr)
    );
    let reply: serde_json::Value =
        serde_json::from_slice(&same.stdout).expect("`status --json` emits one JSON document");
    assert_eq!(reply["worlds"], serde_json::json!([]));

    // Human `status` keeps one sentence and exactly one pid-bearing remedy line.
    let human = client(&beta, &runtime, &state, root, &["status"]);
    let stderr = text(&human.stderr);
    assert!(!human.status.success(), "a skewed client must fail");
    assert!(
        stderr.starts_with("error: the running daemon is"),
        "the refusal names the daemon: {stderr}"
    );
    assert!(
        stderr.contains(&alpha.display().to_string())
            && stderr.contains(&beta.display().to_string()),
        "both binaries are named: {stderr}"
    );
    assert_stale_stderr(&stderr, started_pid);

    // Every machine verb carries the daemon's remedy exactly once, as pure JSON.
    for verb in [
        &["status", "--json"][..],
        &["env", "--json"][..],
        &["logs", "--json"][..],
    ] {
        let output = client(&beta, &runtime, &state, root, verb);
        let value = refusal(&output, verb[0]);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&alpha.display().to_string()),
            "`{}` names the stale daemon: {value}",
            verb[0]
        );
        assert_stale_remedy(value["error"]["next"].as_str().unwrap(), started_pid);
    }

    // The packaged daemon keeps serving its own client while the stranger is refused.
    let still = client(&alpha, &runtime, &state, root, &["status", "--json"]);
    assert!(still.status.success(), "the daemon must survive a refusal");

    // An old-style daemon on a fresh runtime: a real process whose ping has only a pid.
    let legacy_runtime = scratch.runtime("legacy");
    daemons.add(legacy_runtime.clone());
    let legacy_socket = legacy_runtime.join("daemon.sock");
    let legacy_log = root.join("old-daemon.stderr");
    let mut server = Command::new(std::env::current_exe().expect("the test binary path"))
        .args(["--exact", "old_style_daemon_server", "--test-threads=1"])
        .env("SLIPFORK_OLD_DAEMON", "1")
        .env("XDG_RUNTIME_DIR", &legacy_runtime)
        .env("XDG_STATE_HOME", &state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(&legacy_log).expect("old-style daemon stderr log"),
        ))
        .spawn()
        .expect("re-exec the test binary as an old-style daemon");
    let server_pid = server.id();
    wait_for_socket(&legacy_socket, &legacy_log, &mut server);

    // The refusal says it predates this CLI, names both sides, and kills this exact process.
    let legacy = client(&delta, &legacy_runtime, &state, root, &["status", "--json"]);
    let value = refusal(&legacy, "status");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("predates this CLI"), "{message}");
    assert!(
        message.contains(&delta.display().to_string()),
        "the refusal names this CLI: {message}"
    );
    assert!(
        message.contains(env!("CARGO_PKG_VERSION")),
        "the refusal names this CLI version: {message}"
    );
    assert_stale_remedy(value["error"]["next"].as_str().unwrap(), server_pid);

    let legacy_human = client(&delta, &legacy_runtime, &state, root, &["status"]);
    let legacy_stderr = text(&legacy_human.stderr);
    assert!(!legacy_human.status.success(), "the old daemon must refuse");
    assert!(
        legacy_stderr.contains("predates this CLI"),
        "the human refusal says it predates this CLI: {legacy_stderr}"
    );
    assert_stale_stderr(&legacy_stderr, server_pid);
    assert!(
        !legacy_stderr.contains("systemctl"),
        "a pid-bearing refusal never sends the user back to the unit: {legacy_stderr}"
    );

    // Kill the stale process: the next `status --json` spawns this binary's daemon and connects.
    // SAFETY: `kill(2)` on this test's own live child; `wait` below reaps it.
    unsafe {
        libc::kill(server_pid as i32, libc::SIGKILL);
    }
    let killed = server.wait().expect("reap the old-style daemon");
    assert!(!killed.success(), "SIGKILL must not look clean: {killed}");

    let recovered = client(&delta, &legacy_runtime, &state, root, &["status", "--json"]);
    let recovered_err = text(&recovered.stderr);
    assert!(
        recovered.status.success(),
        "after the kill the CLI spawns its own daemon: {}{}",
        text(&recovered.stdout),
        recovered_err
    );
    let reply: serde_json::Value = serde_json::from_slice(&recovered.stdout)
        .expect("`status --json` emits one JSON document after respawn");
    assert_eq!(reply["worlds"], serde_json::json!([]));
    assert!(
        !recovered_err.contains("daemon-skew"),
        "the daemon it just started must match: {recovered_err}"
    );

    // With no daemon at all, `env` spawns this very binary, whose ping must match its identity:
    // the typed `no-forkfile` refusal proves the RPC ran through the fresh daemon.
    let spawned_runtime = scratch.runtime("spawned");
    daemons.add(spawned_runtime.clone());
    let first = client(&gamma, &spawned_runtime, &state, root, &["env", "--json"]);
    let spawned_err = text(&first.stderr);
    assert!(
        !first.status.success(),
        "no Forkfile here, so `env` refuses"
    );
    assert!(
        spawned_err.contains("ephemeral"),
        "the auto-spawn path announces itself: {spawned_err}"
    );
    assert!(
        !spawned_err.contains("daemon-skew"),
        "the daemon it just started must match: {spawned_err}"
    );
    let reply: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("`env --json` emits one JSON document");
    assert_eq!(reply["error"]["code"], "no-forkfile", "{reply}");

    // The second invocation reuses the spawned daemon: no hint, and still no identity refusal.
    let second = client(&gamma, &spawned_runtime, &state, root, &["env", "--json"]);
    let reused_err = text(&second.stderr);
    assert!(
        !reused_err.contains("ephemeral"),
        "the daemon is reused, not respawned: {reused_err}"
    );
    assert!(
        !reused_err.contains("daemon-skew"),
        "a reused daemon still matches: {reused_err}"
    );
}
