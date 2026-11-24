//! Two real binaries on one socket: a daemon started from one path must be refused by another.
//!
//! Every process here is real — a copied `slipfork` binary runs as the daemon and a second copy
//! runs as the client. No Btrfs, no pods, and no mock transport: only `ping`, `status`, and the
//! typed `daemon-skew` refusal that stops a stale packaged daemon from serving new code.

use std::fs;
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
    assert_eq!(
        next.matches("next:").count(),
        0,
        "`{verb}` embeds a remedy in its next: {next}"
    );
    assert!(
        next.contains("systemctl --user stop slipfork"),
        "`{verb}` keeps the daemon's own remedy: {next}"
    );
    assert_eq!(
        error.as_object().map_or(0, serde_json::Map::len),
        3,
        "`{verb}` carries one code, one message, one next: {value}"
    );
    value
}

/// Kills every `__daemon` whose environment names this scratch runtime directory.
struct Daemons {
    runtime: PathBuf,
}

impl Drop for Daemons {
    fn drop(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        let scope = self.runtime.as_os_str().as_bytes();
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
            if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
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

#[test]
fn a_stale_daemon_is_refused_and_an_auto_spawned_one_matches() {
    let scratch = Scratch::new("journey");
    let root = scratch.path();
    let state = scratch.state();
    let alpha = copy_binary(root, "slipfork-alpha");
    let beta = copy_binary(root, "slipfork-beta");
    let gamma = copy_binary(root, "slipfork-gamma");

    // A daemon started from one path makes the other path refuse it with exactly one remedy.
    let runtime = scratch.runtime("runtime");
    let _daemons = Daemons {
        runtime: runtime.clone(),
    };
    let socket = runtime.join("daemon.sock");
    let mut started = daemon(&alpha, &runtime, &state);
    wait_for_socket(&socket, &state.join("daemon.stderr"), &mut started);

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

    // Human `status` keeps one sentence and exactly one remedy line.
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
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("next: "))
            .count(),
        1,
        "exactly one remedy line: {stderr}"
    );
    assert!(
        stderr.contains("systemctl --user stop slipfork"),
        "the remedy stops the packaged daemon: {stderr}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "a piped client is plain: {stderr:?}"
    );

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
    }

    // The packaged daemon keeps serving its own client while the stranger is refused.
    let still = client(&alpha, &runtime, &state, root, &["status", "--json"]);
    assert!(still.status.success(), "the daemon must survive a refusal");

    // With no daemon at all, `env` spawns this very binary, whose ping must match its identity:
    // the typed `no-forkfile` refusal proves the RPC ran through the fresh daemon.
    let spawned_runtime = scratch.runtime("spawned");
    let _spawned_daemons = Daemons {
        runtime: spawned_runtime.clone(),
    };
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
