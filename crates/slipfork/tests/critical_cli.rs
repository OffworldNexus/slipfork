//! Protected end-to-end CLI + `slipforkd` critical tests (`H-010` #1, `ADR-0008` §4).
//!
//! These tests boot real `ReflinkNetnsEngine` worlds through a real `slipforkd` child process,
//! drive the thin `slipfork` binary over the `0600` `<runtime_dir>/daemon.sock`, and assert the
//! adversarial contracts: fail-closed `HOME`, exactly one URL per world, idempotent `down`, the
//! `share`/`unshare` asymmetry, secret overwrite refusal, and non-template unit rendering.
//!
//! The `slipfork-world-init` helper is located in Cargo's target directory; when the engine's bins
//! were not built by the current invocation, the test builds them once (the cargo build lock is
//! free while a test binary runs).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const SNOW_YML: &str = r#"
images:
  - id: app
    registry: docker.io
    repository: library/alpine
    tag: "3.20"

components:
  - id: api
    image: app
    envs: [app]
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

/// Disk-backed scratch directory with a short path (AF_UNIX `sun_path` budget), never `/tmp`.
fn disk_tempdir() -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME")
        .join(".cache")
        .join("slipfork-cli-tests");
    fs::create_dir_all(&base).expect("create test cache base");
    tempfile::Builder::new()
        .prefix("c-")
        .tempdir_in(base)
        .expect("create disk tempdir")
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Slipfork Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
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

/// One git repository with the canonical `snow.yml` used by every test.
fn repo_with_snow(scratch: &Path) -> PathBuf {
    let repo = scratch.join("acme-app");
    fs::create_dir_all(&repo).expect("create repo");
    run_git(&repo, &["init", "-q"]);
    fs::write(repo.join("snow.yml"), SNOW_YML).expect("write snow.yml");
    fs::write(repo.join("slipfork.toml"), "project = \"acme-app\"\n").expect("write slipfork.toml");
    run_git(&repo, &["add", "snow.yml", "slipfork.toml"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Locate (or build once) the engine's `slipfork-world-init` helper next to the test binaries.
fn world_init_helper() -> PathBuf {
    // `CARGO_BIN_EXE_slipforkd` is the canonical installed-daemon path for this test build;
    // `current_exe()` may live in a build-script output directory, not `<target>/debug`.
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
struct Daemon {
    child: Child,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
}

impl Daemon {
    fn start(scratch: &Path) -> Self {
        let state_dir = scratch.join("state");
        let runtime_dir = scratch.join("rt");
        let log_path = scratch.join("daemon.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .expect("open daemon log");
        let stderr = log.try_clone().expect("clone log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipforkd"))
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--world-init")
            .arg(world_init_helper())
            .env("XDG_DATA_HOME", state_dir.join("xdg-data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("spawn slipforkd");
        let socket = runtime_dir.join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if UnixStream::connect(&socket).is_ok() {
                return Self {
                    child,
                    state_dir,
                    runtime_dir,
                };
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "slipforkd exited with {status} before serving {}:\n{}",
                    socket.display(),
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "slipforkd did not serve {} within 20s:\n{}",
            socket.display(),
            fs::read_to_string(&log_path).unwrap_or_default()
        );
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--runtime-dir")
            .arg(&self.runtime_dir)
            .args(args)
            .current_dir(cwd)
            .env("XDG_DATA_HOME", self.state_dir.join("xdg-data"))
            .output()
            .expect("spawn slipfork")
    }

    fn cli_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--runtime-dir")
            .arg(&self.runtime_dir)
            .args(args)
            .current_dir(cwd)
            .env("XDG_DATA_HOME", self.state_dir.join("xdg-data"))
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
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn url_count(text: &str) -> usize {
    text.matches("http://").count()
}

#[test]
fn up_down_lifecycle_is_idempotent_and_one_url_per_world() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);

    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "slipfork up");
    assert_eq!(url_count(&stdout(&up)), 1, "up prints exactly one origin");
    assert!(
        stdout(&up).contains("[origin]"),
        "expected an [origin] line: {}",
        stdout(&up)
    );

    let list = daemon.cli(&repo, &["list"]);
    assert_success(&list, "slipfork list");
    assert_eq!(
        url_count(&stdout(&list)),
        1,
        "list prints exactly one URL for one world:\n{}",
        stdout(&list)
    );

    // Re-running `up` is idempotent and never mints a second URL.
    let up_again = daemon.cli(&repo, &["up"]);
    assert_success(&up_again, "second slipfork up");
    assert_eq!(url_count(&stdout(&up_again)), 1);

    // Direct daemon-resolved execution enters the live world with inherited stdio.
    let exec = daemon.cli(&repo, &["exec", "--", "true"]);
    assert_success(&exec, "slipfork exec -- true");

    let prune = daemon.cli(&repo, &["prune", "--dry-run"]);
    assert_success(&prune, "slipfork prune --dry-run");

    let down = daemon.cli(&repo, &["down"]);
    assert_success(&down, "slipfork down");
    assert!(
        stdout(&down).contains("[down]"),
        "expected a down line: {}",
        stdout(&down)
    );

    // A second `down` released nothing but is still a success (idempotent).
    let down_again = daemon.cli(&repo, &["down"]);
    assert_success(&down_again, "second slipfork down");
    assert!(
        stdout(&down_again).contains("nothing to do"),
        "expected idempotent second down: {}",
        stdout(&down_again)
    );
}

#[test]
fn one_url_per_world_holds_for_canonical_and_fork() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);

    assert_success(&daemon.cli(&repo, &["up"]), "up canonical");
    let fork = daemon.cli(&repo, &["fork", "feat-x"]);
    assert_success(&fork, "fork feat-x");
    assert!(
        repo.join(".worktrees/feat-x/snow.yml").is_file(),
        "fork must create the worktree checkout"
    );

    let json_list = daemon.cli(&repo, &["--json", "list"]);
    assert_success(&json_list, "list --json");
    let worlds: Value = serde_json::from_str(&stdout(&json_list)).expect("list json");
    let worlds = worlds.as_array().expect("list is an array");
    assert_eq!(worlds.len(), 2, "canonical plus one fork");
    for world in worlds {
        let origin = world["origin"].as_str().expect("origin string");
        assert_eq!(
            origin.matches("http://").count(),
            1,
            "every world must carry exactly one origin URL: {origin}"
        );
        assert!(world["worktree"].as_str().is_some(), "worktree path");
    }

    let human_list = daemon.cli(&repo, &["list"]);
    assert_success(&human_list, "list");
    assert_eq!(
        url_count(&stdout(&human_list)),
        2,
        "one URL per world, asserted by line/URL count:\n{}",
        stdout(&human_list)
    );

    let drop = daemon.cli(&repo, &["drop", "feat-x"]);
    assert_success(&drop, "drop feat-x");
    assert!(
        !repo.join(".worktrees/feat-x").exists(),
        "drop must remove the fork worktree"
    );
    let after = daemon.cli(&repo, &["--json", "list"]);
    assert_success(&after, "list after drop");
    let remaining: Value = serde_json::from_str(&stdout(&after)).expect("json");
    let remaining = remaining.as_array().expect("array");
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining[0]["origin"]
            .as_str()
            .expect("origin")
            .matches("http://")
            .count(),
        1
    );
}

#[test]
fn home_unset_fails_closed_before_any_mutation() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);

    // Explicit roots make the "mutates nothing" assertion precise: the HOME guard must fire before
    // either directory is created.
    let state_dir = scratch.join("home-unset-state");
    let runtime_dir = scratch.join("home-unset-rt");
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--runtime-dir")
        .arg(&runtime_dir)
        .arg("up")
        .current_dir(&repo)
        .env_remove("HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("spawn slipfork without HOME");
    assert!(!output.status.success(), "HOME unset must fail closed");
    assert!(
        stderr(&output).contains("HOME is not set"),
        "expected an actionable HOME diagnostic, got: {}",
        stderr(&output)
    );
    assert!(
        !state_dir.exists() && !runtime_dir.exists(),
        "a HOME-unset invocation must not create state or runtime directories"
    );
}

#[test]
fn share_blocked_while_unshare_is_an_idempotent_no_op() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up");

    // `--listen` binds no forward listener, so `share` fails closed before any tunnel attempt.
    let share = daemon.cli(&repo, &["share"]);
    assert!(!share.status.success(), "share must fail closed");
    assert!(
        stderr(&share).contains("forward listener"),
        "expected the forward-listener diagnostic, got: {}",
        stderr(&share)
    );

    let stop = daemon.cli(&repo, &["share", "--stop"]);
    assert_success(&stop, "share --stop on a local world");
    let stop_again = daemon.cli(&repo, &["share", "--stop"]);
    assert_success(&stop_again, "second share --stop is idempotent");
}

#[test]
fn secret_set_refuses_an_existing_double_bang_key_and_never_echoes_values() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up generates the !! secret");

    let vault = daemon
        .state_dir
        .join("xdg-data/slipfork/vaults/acme-app.json");
    assert!(vault.is_file(), "the up run must persist the !! secret");
    let mode = fs::metadata(&vault)
        .expect("vault metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the vault must stay 0600");

    let refused = daemon.cli_with_stdin(&repo, &["secret", "set", "SECRET_KEY"], "new-value-123");
    assert!(
        !refused.status.success(),
        "overwrite without --force must be refused"
    );
    assert!(
        stderr(&refused).contains("--force"),
        "expected a --force hint, got: {}",
        stderr(&refused)
    );
    let original = fs::read_to_string(&vault).expect("vault body");
    assert!(
        !original.contains("new-value-123"),
        "a refused write must not mutate the vault"
    );
    assert!(
        !stdout(&refused).contains("new-value-123") && !stderr(&refused).contains("new-value-123"),
        "secret values must never be echoed"
    );

    let forced = daemon.cli_with_stdin(
        &repo,
        &["secret", "set", "--force", "SECRET_KEY"],
        "new-value-123",
    );
    assert_success(&forced, "secret set --force");
    assert!(
        !stdout(&forced).contains("new-value-123") && !stderr(&forced).contains("new-value-123"),
        "secret values must never be echoed"
    );

    let listed = daemon.cli(&repo, &["secret", "list"]);
    assert_success(&listed, "secret list");
    assert!(
        stdout(&listed).lines().any(|line| line == "SECRET_KEY"),
        "secret list must name the key only: {}",
        stdout(&listed)
    );

    let removed = daemon.cli(&repo, &["secret", "unset", "SECRET_KEY"]);
    assert_success(&removed, "secret unset");
    let after = daemon.cli(&repo, &["secret", "list"]);
    assert_success(&after, "secret list after unset");
    assert!(
        !stdout(&after).lines().any(|line| line == "SECRET_KEY"),
        "the removed key must disappear from the list: {}",
        stdout(&after)
    );
}

#[test]
fn daemon_unit_renders_the_resolved_user_and_never_a_template_specifier() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let expected_user = String::from_utf8(
        Command::new("id")
            .arg("-un")
            .output()
            .expect("id -un")
            .stdout,
    )
    .expect("utf8 account name");
    let expected_user = expected_user.trim();
    assert!(!expected_user.is_empty());

    let service = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["daemon", "unit", "--only", "service"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("daemon unit --only service");
    assert_success(&service, "daemon unit --only service");
    let service_text = stdout(&service);
    assert!(
        service_text.contains(&format!("User={expected_user}\n")),
        "expected the resolved developer account, got:\n{service_text}"
    );
    assert!(
        !service_text
            .lines()
            .any(|line| line == "User=" || line.contains("User=%")),
        "systemd template/empty users are forbidden:\n{service_text}"
    );

    let socket = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["daemon", "unit", "--only", "socket"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("daemon unit --only socket");
    assert_success(&socket, "daemon unit --only socket");
    assert_eq!(
        stdout(&socket)
            .lines()
            .filter(|line| line.starts_with("ListenStream="))
            .count(),
        3,
        "two ingress listeners plus the planned forward listener:\n{}",
        stdout(&socket)
    );

    let collision = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["daemon", "unit", "--only", "socket", "--forward-port", "80"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("daemon unit collision");
    assert!(
        !collision.status.success(),
        "a forward port colliding with :80 must fail closed"
    );
    assert!(
        stderr(&collision).contains("collides"),
        "expected the unit port-collision diagnostic, got: {}",
        stderr(&collision)
    );

    // Rendering units is a pure stdout operation: no state or runtime directory is created.
    assert!(
        !scratch.join("state").exists() && !scratch.join("rt").exists(),
        "daemon unit must not write any files"
    );
}

#[test]
fn zero_setup_auto_spawn_serves_up_without_a_prestarted_daemon() {
    use std::os::unix::process::CommandExt as _;

    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let state_dir = scratch.join("auto-state");
    let runtime_dir = scratch.join("auto-rt");
    let daemon_log = runtime_dir.join("daemon.log");

    let child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--runtime-dir")
        .arg(&runtime_dir)
        .args(["up"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-auto-data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The CLI and the daemon it spawns share this new process group, so the test can reap the
        // detached zero-setup daemon without inventing a pid file or a `daemon stop` command.
        .process_group(0)
        .spawn()
        .expect("spawn slipfork (auto-spawn)");
    let process_group = child.id();
    let output = child.wait_with_output().expect("wait for auto-spawned up");
    assert_success(&output, "auto-spawned slipfork up");
    assert_eq!(url_count(&stdout(&output)), 1);

    // The daemon socket and its log are user-private.
    let mode = fs::metadata(runtime_dir.join("daemon.sock"))
        .expect("daemon socket")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon.sock must be 0600");
    assert!(daemon_log.is_file(), "auto-spawn must record its log");

    let killed = Command::new("kill")
        .arg("-TERM")
        .arg("--")
        .arg(format!("-{process_group}"))
        .status()
        .expect("send SIGTERM to the auto-spawn process group");
    assert!(
        killed.success(),
        "kill -TERM -{process_group} must succeed to prove the test reaped its daemon"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if UnixStream::connect(runtime_dir.join("daemon.sock")).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        UnixStream::connect(runtime_dir.join("daemon.sock")).is_err(),
        "the auto-spawned slipforkd must stop accepting on {} within 10s",
        runtime_dir.join("daemon.sock").display()
    );
}
