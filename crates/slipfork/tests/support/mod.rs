//! Shared integration-test harness for the thin CLI and `git-remote-slipfork`.
//!
//! Every helper is real: a disk-backed scratch directory (never tmpfs), a real Git repository, a
//! real `slipforkd` child with the engine's `slipfork-world-init` helper, and the actual `slipfork`
//! binary. The daemon's runtime directory lives at `$XDG_RUNTIME_DIR/slipfork` so the
//! `git-remote-slipfork` helper resolves exactly the same control socket from the environment.

#![allow(dead_code)]

use std::fs::{self, OpenOptions};
use std::io::BufRead;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use slipfork_edge::DaemonControlClient;
use slipfork_testkit::{DAEMON_STARTUP_DEADLINE, SHUTDOWN_DEADLINE, poll_until};
use tempfile::TempDir;

/// Disk-backed scratch directory with a short path (AF_UNIX `sun_path` budget), never `/tmp`.
pub fn disk_tempdir(prefix: &str) -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME")
        .join(".cache")
        .join("slipfork-cli-tests");
    fs::create_dir_all(&base).expect("create test cache base");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .expect("create disk tempdir")
}

pub fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Slipfork Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            // The suites clone and add local repositories as submodules, which git's default
            // `protocol.file` policy refuses; the fixtures are paths this test created itself.
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test fixture `Dockerfile`: a real supported `L0` stage with the client tools the CLI
/// critical tests exercise. Every world boot builds this through the production
/// `DockerfileStageBuilder`; nothing mocks Docker or the pinned static `crun`.
pub const FIXTURE_DOCKERFILE: &str = "\
FROM alpine:3.21 AS app
RUN apk add --no-cache python3
WORKDIR /workspace
";

/// One git repository carrying an explicit `snow.yml` body and a buildable `Dockerfile`.
pub fn repo_with_manifest(scratch: &Path, project: &str, snow: &str) -> PathBuf {
    let repo = scratch.join(project);
    fs::create_dir_all(&repo).expect("create repo");
    run_git(&repo, &["init", "-q"]);
    fs::write(repo.join("snow.yml"), snow).expect("write snow.yml");
    fs::write(repo.join("Dockerfile"), FIXTURE_DOCKERFILE).expect("write Dockerfile");
    fs::write(
        repo.join("slipfork.toml"),
        format!("project = \"{project}\"\n"),
    )
    .expect("write slipfork.toml");
    run_git(&repo, &["add", "snow.yml", "slipfork.toml", "Dockerfile"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Locate (or build once) the engine's `slipfork-world-init` helper next to the test binaries.
pub fn world_init_helper() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_slipforkd"));
    let helper = daemon
        .parent()
        .expect("daemon binary parent")
        .join("slipfork-world-init");
    if !helper.is_file() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "slipfork-engine", "--bins"])
            .status()
            .expect("build slipfork-engine bins");
        assert!(
            status.success(),
            "cargo build -p slipfork-engine --bins failed"
        );
    }
    assert!(
        helper.is_file(),
        "expected the world-init helper at {}",
        helper.display()
    );
    helper
}

/// A live `slipforkd` child plus the explicit roots it serves.
pub struct Daemon {
    pub child: Child,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub browser_marker: PathBuf,
}

impl Daemon {
    pub fn start(scratch: &Path) -> Self {
        Self::start_with_path(scratch, None)
    }

    /// Start the daemon with an explicit `PATH` (used to prove `.jj` routing fails closed when
    /// `jj` is absent, and succeeds when it is present).
    pub fn start_with_path(scratch: &Path, path: Option<&Path>) -> Self {
        let state_dir = scratch.join("state");
        let xdg_runtime = scratch.join("xdg-runtime");
        let runtime_dir = xdg_runtime.join("slipfork");
        fs::create_dir_all(&runtime_dir).expect("create runtime dir");
        let log_path = scratch.join("daemon.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .expect("open daemon log");
        let stderr = log.try_clone().expect("clone log");
        let (browser, browser_marker) = browser_opener(scratch);
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipforkd"));
        command
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--world-init")
            .arg(world_init_helper())
            .arg("--crun")
            .arg(pinned_crun())
            .env("XDG_DATA_HOME", state_dir.join("xdg-data"))
            .env("XDG_RUNTIME_DIR", &xdg_runtime)
            .env("BROWSER", browser)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let mut child = command.spawn().expect("spawn slipforkd");
        let client = DaemonControlClient::bind(&runtime_dir);
        let served = poll_until(DAEMON_STARTUP_DEADLINE, || {
            if client.is_served() {
                return Ok(());
            }
            // An exited child is terminal, not a retryable state: fail now with its log instead of
            // sleeping through the whole deadline.
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "slipforkd exited with {status} before serving {}:\n{}",
                    client.socket_path().display(),
                    fs::read_to_string(&log_path)
                        .unwrap_or_else(|error| format!("<logs unavailable: {error}>"))
                );
            }
            Err(format!(
                "{} is not served yet",
                client.socket_path().display()
            ))
        });
        if let Err(last) = served {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "slipforkd did not serve {} within {DAEMON_STARTUP_DEADLINE:?} ({last}):\n{}",
                client.socket_path().display(),
                fs::read_to_string(&log_path)
                    .unwrap_or_else(|error| format!("<logs unavailable: {error}>"))
            );
        }
        Self {
            child,
            state_dir,
            runtime_dir,
            browser_marker,
        }
    }

    pub fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn slipfork")
    }

    pub fn cli_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .command(cwd, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("wait for slipfork")
    }

    /// Spawn a streaming CLI command with piped stdout (for `logs --follow` / `forward`).
    pub fn cli_spawn(&self, cwd: &Path, args: &[&str]) -> Child {
        self.command(cwd, args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork stream")
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--runtime-dir")
            .arg(&self.runtime_dir)
            .args(args)
            .current_dir(cwd)
            .env("XDG_DATA_HOME", self.state_dir.join("xdg-data"))
            .env(
                "XDG_RUNTIME_DIR",
                self.runtime_dir.parent().expect("runtime parent"),
            );
        command
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn browser_opener(scratch: &Path) -> (PathBuf, PathBuf) {
    let marker = scratch.join("browser-used");
    let script = scratch.join("browser.sh");
    fs::write(&script, format!("#!/bin/sh\n: > '{}'\n", marker.display()))
        .expect("write browser stub");
    let mut permissions = fs::metadata(&script)
        .expect("browser stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("make browser stub executable");
    (script, marker)
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

/// Wait until the daemon serving `runtime_dir` stops accepting on `<runtime_dir>/daemon.sock`.
///
/// Probes through the production [`DaemonControlClient::is_served`] the CLI's zero-setup auto-spawn
/// uses, so this proof can never drift from the check a later invocation would really make.
/// Returns the still-served socket description once [`SHUTDOWN_DEADLINE`] expires, so each caller
/// panics with its own captured invocation output.
pub fn wait_daemon_teardown(runtime_dir: &Path) -> Result<(), String> {
    let client = DaemonControlClient::bind(runtime_dir);
    let socket = client.socket_path().to_owned();
    poll_until(SHUTDOWN_DEADLINE, || {
        if client.is_served() {
            Err(format!("{} still accepts connections", socket.display()))
        } else {
            Ok(())
        }
    })
    .map_err(|last| format!("{last} after {SHUTDOWN_DEADLINE:?}"))
}

/// Bound on one streamed child line.
///
/// This is the one legitimate non-poll wait: the producer thread blocks on the child's stdout pipe
/// and the consumer must block on the channel until the next line arrives, so a `poll_until` loop
/// would only re-implement `recv_timeout` on the same channel. The blocking bound is therefore a
/// named deadline, not a polling interval.
const STREAM_LINE_DEADLINE: Duration = Duration::from_secs(30);

/// Read lines from a child's stdout until `predicate` accepts one or the deadline expires.
pub fn wait_for_line(child: &mut Child, predicate: impl Fn(&str) -> bool) -> String {
    let stdout = child.stdout.take().expect("child stdout");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + STREAM_LINE_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(line) if predicate(&line) => return line,
            Ok(_) => {}
            Err(error) => panic!(
                "streaming command produced no matching line within {STREAM_LINE_DEADLINE:?}: {error}"
            ),
        }
    }
}

/// Compile-time pin mirrored for the CLI tests (`slipfork-engine::rootfs::pin` is the source).
pub const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
pub const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
pub const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";

/// The pinned static `crun`, downloaded once into `~/.cache/slipfork/test-runtime` and verified.
pub fn pinned_crun() -> PathBuf {
    let cache = PathBuf::from(std::env::var("HOME").expect("HOME"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime");
    fs::create_dir_all(&cache).expect("create runtime cache");
    let path = cache.join(CRUN_FILE_NAME);
    if verified(&path) {
        return path;
    }
    let temporary = cache.join(format!("{CRUN_FILE_NAME}.download"));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(CRUN_URL)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to fetch the pinned static crun: {error}\n  fix: install curl, or place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
                cache.display()
            )
        });
    assert!(status.success(), "failed to download {CRUN_URL}");
    assert!(
        verified(&temporary),
        "downloaded crun does not match the compile-time pin {CRUN_SHA256}"
    );
    fs::rename(&temporary, &path).expect("install pinned crun");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod crun");
    path
}

fn verified(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes)) == CRUN_SHA256
}

/// Locate one executable on the current `PATH`.
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// The Jujutsu binary: `jj` on `PATH`, else the developer cache used by this workspace.
pub fn jj_binary() -> Option<PathBuf> {
    find_on_path("jj").or_else(|| {
        let cached = PathBuf::from(std::env::var_os("HOME")?)
            .join(".cache")
            .join("slipfork-tools")
            .join("jj")
            .join("jj");
        cached.is_file().then_some(cached)
    })
}

/// A directory exposing only `git` (so a `.jj` repository can be resolved while `jj` is absent).
pub fn git_only_path(scratch: &Path) -> PathBuf {
    let directory = scratch.join("git-only-bin");
    fs::create_dir_all(&directory).expect("create git-only bin dir");
    let git = find_on_path("git").expect("git must be on PATH for the .jj routing test");
    std::os::unix::fs::symlink(&git, directory.join("git")).expect("symlink git");
    directory
}
