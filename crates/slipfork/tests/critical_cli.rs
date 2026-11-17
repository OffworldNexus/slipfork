//! Protected end-to-end CLI + `slipforkd` critical tests (`H-010` #1, `ADR-0008` §4).
//!
//! These tests boot real `BtrfsEngine` worlds through the harness `slipforkd` composition root
//! (the real child on a Btrfs host, the in-process `TestTreeDriver` backend elsewhere), drive the
//! thin `slipfork` binary over the `0600` `<runtime_dir>/daemon.sock`, and assert the adversarial
//! contracts: fail-closed `HOME`, exactly one URL per world, idempotent `down`, the
//! `share`/`unshare` asymmetry, secret overwrite refusal, and non-template unit rendering.
//!
//! The `slipfork-world-init` helper is located in Cargo's target directory; when the engine's bins
//! were not built by the current invocation, the test builds them once (the cargo build lock is
//! free while a test binary runs).

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use slipfork_testkit::{SHUTDOWN_DEADLINE, poll_until};

pub mod support;
use support::{Daemon, assert_success, repo_with_manifest, stderr, stdout, wait_for_line};

const SNOW_YML: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

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
fn disk_tempdir() -> tempfile::TempDir {
    support::disk_tempdir("c-")
}

/// One git repository with the canonical `snow.yml` used by every test.
fn repo_with_snow(scratch: &Path) -> PathBuf {
    repo_with_manifest(scratch, "acme-app", SNOW_YML)
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
    let canonical_id = worlds
        .iter()
        .find(|world| world["worktree"].as_str() == Some(repo.to_string_lossy().as_ref()))
        .and_then(|world| world["world"].as_str())
        .expect("canonical world row")
        .to_owned();
    let fork_worktree = repo.join(".worktrees/feat-x");
    let fork_id = worlds
        .iter()
        .find(|world| world["worktree"].as_str() == Some(fork_worktree.to_string_lossy().as_ref()))
        .and_then(|world| world["world"].as_str())
        .expect("fork world row")
        .to_owned();
    assert_eq!(canonical_id, "acme-app");
    assert_eq!(fork_id, "acme-app:feat-x");

    let human_list = daemon.cli(&repo, &["list"]);
    assert_success(&human_list, "list");
    assert_eq!(
        url_count(&stdout(&human_list)),
        2,
        "one URL per world, asserted by line/URL count:\n{}",
        stdout(&human_list)
    );
    assert!(
        stdout(&human_list).contains(&fork_id) && stdout(&human_list).contains(&canonical_id),
        "the console prints the same ids `--world` accepts:\n{}",
        stdout(&human_list)
    );

    // The exact strings `list` prints round-trip through `--world` (0.1.0 minimal selector).
    let exec_world = |selector: &str| -> Value {
        let output = daemon.cli(
            &repo,
            &["--json", "exec", "--world", selector, "--", "true"],
        );
        assert_success(&output, &format!("exec --world {selector}"));
        serde_json::from_str(&stdout(&output)).expect("exec json")
    };
    assert_eq!(exec_world(&canonical_id)["world"], canonical_id);
    assert_eq!(exec_world(&fork_id)["world"], fork_id);
    assert_eq!(exec_world("feat-x")["world"], fork_id);

    // A foreign project prefix and more than one `:` fail closed with a typed diagnostic.
    let foreign = daemon.cli(
        &repo,
        &["--json", "exec", "--world", "other:feat-x", "--", "true"],
    );
    assert!(!foreign.status.success(), "a foreign project must fail");
    let foreign = stdout(&foreign);
    assert!(
        foreign.contains("belongs to project 'other'") && foreign.contains("acme-app"),
        "the foreign-project refusal must name both projects: {foreign}"
    );
    let multi = daemon.cli(
        &repo,
        &["--json", "exec", "--world", "acme-app:feat:x", "--", "true"],
    );
    assert!(!multi.status.success(), "two separators must fail");
    assert!(
        stdout(&multi).contains("at most one ':'"),
        "the multi-separator refusal must name the grammar: {}",
        stdout(&multi)
    );

    // `drop` accepts the same printed id.
    let drop = daemon.cli(&repo, &["drop", fork_id.as_str()]);
    assert_success(&drop, "drop the printed fork id");
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
    fs::create_dir_all(&runtime_dir).expect("create runtime dir");

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

    if !support::host_is_btrfs(&state_dir) {
        // Off Btrfs the production daemon refuses this state root before any world exists: the
        // zero-setup path must surface that refusal, never a degraded unisolated world
        // (`ADR-0010` §4). The full auto-spawn happy path runs on the Btrfs VM.
        assert!(
            !output.status.success(),
            "a non-Btrfs state root must refuse the world boot:\n{}",
            stdout(&output)
        );
        let diagnostic = stderr(&output);
        assert!(
            diagnostic.contains("slipforkd exited with exit status: 1 before serving"),
            "the CLI must report the refused zero-setup daemon: {diagnostic}"
        );
        assert!(
            diagnostic.contains(daemon_log.to_string_lossy().as_ref()),
            "the CLI must point at the daemon log: {diagnostic}"
        );
        assert!(
            daemon_log.is_file(),
            "the auto-spawn must still record the daemon log"
        );
        let log = fs::read_to_string(&daemon_log).expect("daemon log");
        support::assert_btrfs_refusal(&log);
        assert!(
            !runtime_dir.join("daemon.sock").exists(),
            "a refused daemon never binds its control socket"
        );
        return;
    }

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
    support::wait_daemon_teardown(&runtime_dir)
        .unwrap_or_else(|error| panic!("the auto-spawned slipforkd must stop accepting: {error}"));
}

// ---------------------------------------------------------------------------
// M2.17: hook rescue UX, logs, shell, db/redis, forward, auth, support
// ---------------------------------------------------------------------------

const HOOK_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

hooks:
  - component: api
    when: pre-deploy
    run_command: "echo HOOK-BOOM; echo HOOK-ERR >&2; exit 7"

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

const LOGS_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "while true; do echo tick-line; sleep 0.1; done"

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
"#;

const FORWARD_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    internal_ports: [8000]
    run_command: "exec python3 -m http.server 8000 --bind 127.0.0.1"

  - id: idle
    image: app
    internal_ports: [8001]
    # Accepts the readiness probe, then closes the listener: the forwarded target is dead by the
    # time the test connects, without weakening the bridge-isolation assertion.
    run_command: 'exec python3 -c "import socket,time; s=socket.socket(); s.bind((\"127.0.0.1\",8001)); s.listen(1); s.accept(); s.close(); time.sleep(300)"' 

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
"#;

const DB_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

resources:
  pg_clusters:
    - id: main
      version: "18"
  pg_databases:
    - id: app
      cluster: main
  redis_clusters:
    - id: cache
      version: "8"

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

fn find_log_file(root: &Path, needle: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_log_file(&path, needle) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|ext| ext == "log")
            && fs::read_to_string(&path)
                .map(|body| body.contains(needle))
                .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

#[test]
fn hook_failure_keeps_the_world_and_prints_the_full_diagnostic() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-hook", HOOK_SNOW);
    let daemon = Daemon::start(scratch);

    let fork = daemon.cli(&repo, &["fork", "feat-bad"]);
    assert!(!fork.status.success(), "a failing hook must fail `fork`");
    let out = stdout(&fork);
    let err = stderr(&fork);
    assert!(
        out.contains("HOOK-BOOM"),
        "hook stdout must be streamed: {out}"
    );
    assert!(
        err.contains("HOOK-ERR"),
        "hook stderr must be streamed untruncated: {err}"
    );
    assert!(
        err.contains("api:pre-deploy") && err.contains("exit") && err.contains("7"),
        "the exact failing hook and exit code must be printed: {err}"
    );
    assert!(
        err.contains("/logs/") && err.contains(".log"),
        "the rescue block must point at a real log file: {err}"
    );
    assert!(
        err.contains("Rescue commands") && err.contains("--world feat-bad"),
        "the rescue block must name the kept fork: {err}"
    );

    let listed = daemon.cli(&repo, &["--json", "list"]);
    assert_success(&listed, "list after a failed hook");
    let worlds: Value = serde_json::from_str(&stdout(&listed)).expect("list json");
    let failed = worlds
        .as_array()
        .expect("array")
        .iter()
        .find(|world| {
            world["world"]
                .as_str()
                .is_some_and(|id| id.ends_with(":feat-bad"))
        })
        .expect("the failed fork must stay listed");
    assert_eq!(failed["state"], "failed");

    let log = find_log_file(&daemon.state_dir, "HOOK-BOOM")
        .expect("the failed hook must retain a real log file with its output");
    assert!(
        fs::read_to_string(&log)
            .expect("log body")
            .contains("HOOK-ERR"),
        "both streams must be retained in the log file"
    );
}

#[test]
fn rescue_commands_re_run_and_inspect_the_failed_world() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-rescue", HOOK_SNOW);
    let daemon = Daemon::start(scratch);

    assert!(!daemon.cli(&repo, &["fork", "feat-bad"]).status.success());

    let rerun = daemon.cli(
        &repo,
        &[
            "hook",
            "run",
            "api:pre-deploy",
            "--world",
            "feat-bad",
            "--no-tty",
        ],
    );
    assert!(!rerun.status.success(), "the failing hook keeps failing");
    assert!(
        stdout(&rerun).contains("HOOK-BOOM"),
        "hook run --no-tty must stream output: {}",
        stdout(&rerun)
    );

    let hook_logs = daemon.cli(&repo, &["logs", "--world", "feat-bad", "api:pre-deploy"]);
    assert_success(&hook_logs, "logs of a failed hook");
    assert!(
        stdout(&hook_logs).contains("HOOK-BOOM"),
        "hook logs must be readable: {}",
        stdout(&hook_logs)
    );

    let unknown = daemon.cli(&repo, &["logs", "--world", "feat-bad", "nope"]);
    assert!(!unknown.status.success(), "an unknown log target must fail");
    assert!(
        stderr(&unknown).contains("known targets"),
        "the diagnostic must list valid targets: {}",
        stderr(&unknown)
    );

    let shell = daemon.cli(&repo, &["shell", "--world", "feat-bad"]);
    assert!(
        !shell.status.success(),
        "shell without a TTY must fail closed"
    );
    assert!(
        stderr(&shell).contains("interactive terminal"),
        "the diagnostic must name the TTY requirement: {}",
        stderr(&shell)
    );

    let interactive = daemon.cli(
        &repo,
        &["hook", "run", "api:pre-deploy", "--world", "feat-bad"],
    );
    assert!(
        !interactive.status.success(),
        "interactive hook run without a TTY must fail closed"
    );
    assert!(
        stderr(&interactive).contains("--no-tty"),
        "the failure must name `hook run --no-tty`: {}",
        stderr(&interactive)
    );
}

#[test]
fn logs_follow_streams_new_component_output() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-logs", LOGS_SNOW);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up for logs");

    let mut followed = daemon.cli_spawn(&repo, &["logs", "api", "--follow"]);
    let line = wait_for_line(&mut followed, |line| line.contains("tick-line"));
    assert!(line.contains("tick-line"));
    let _ = followed.kill();
    let _ = followed.wait();

    let tail = daemon.cli(&repo, &["logs", "api", "--lines", "1"]);
    assert_success(&tail, "logs --lines 1");
    assert_eq!(
        stdout(&tail)
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
        1,
        "the retained tail must be exactly one line: {}",
        stdout(&tail)
    );
}

#[test]
fn forward_bridge_streams_a_real_request_and_closes_on_disconnect() {
    use std::io::Read;
    use std::net::TcpStream;

    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-forward", FORWARD_SNOW);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up for forward");

    let mut forward = daemon.cli_spawn(
        &repo,
        &["--json", "forward", "components.api", "components.idle"],
    );
    let line = wait_for_line(&mut forward, |line| line.contains("host_addr"));
    let summary: Value = serde_json::from_str(&line).expect("forward json");
    let targets = summary["targets"].as_array().expect("targets");
    let api = targets
        .iter()
        .find(|target| target["id"] == "components.api")
        .expect("components.api target");
    let dead = targets
        .iter()
        .find(|target| target["id"] == "components.idle")
        .expect("s3 target");
    let api_addr = api["host_addr"].as_str().expect("api host addr");
    let dead_addr = dead["host_addr"].as_str().expect("dead host addr");

    let mut stream = TcpStream::connect(api_addr).expect("connect to the forwarded component");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: slipfork\r\n\r\n")
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(
        response.contains("200 OK"),
        "the daemon-owned bridge must reach the world listener: {response}"
    );

    // A target whose world port does not listen closes that one connection and nothing else.
    let mut dead_stream = TcpStream::connect(dead_addr).expect("connect to the dead target");
    dead_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("dead read timeout");
    let mut sink = Vec::new();
    let _ = dead_stream.read_to_end(&mut sink);
    let mut still = TcpStream::connect(api_addr).expect("bridge survives a dead target");
    still
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("still read timeout");
    still
        .write_all(b"GET / HTTP/1.0\r\nHost: slipfork\r\n\r\n")
        .expect("still write");
    let mut again = String::new();
    still.read_to_string(&mut again).expect("still read");
    assert!(again.contains("200 OK"));

    let _ = forward.kill();
    let _ = forward.wait();
    // The daemon-owned listener closes only once the CLI process is gone; wait boundedly so the
    // proof is deterministic instead of racing the disconnect.
    poll_until(SHUTDOWN_DEADLINE, || {
        if TcpStream::connect(api_addr).is_err() {
            Ok(())
        } else {
            Err(format!("{api_addr} still accepts connections"))
        }
    })
    .unwrap_or_else(|last| {
        panic!(
            "the host listener must close within {SHUTDOWN_DEADLINE:?} when the CLI disconnects: {last}"
        )
    });
}

#[test]
fn db_redis_exec_resolve_container_entries_and_require_a_tty() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-db", DB_SNOW);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up for db/redis");

    // Both clients resolve through the running service container, then refuse without a TTY
    // instead of ever falling back to the app rootfs.
    let db = daemon.cli(&repo, &["db"]);
    assert!(!db.status.success(), "a non-TTY db entry must fail closed");
    let diagnostic = stderr(&db);
    assert!(
        diagnostic.contains("needs an interactive terminal"),
        "the resolved container entry must require a TTY: {diagnostic}"
    );
    assert!(
        !diagnostic.contains("ServiceNotRunning"),
        "a running postgres container must not report ServiceNotRunning: {diagnostic}"
    );
    assert!(
        stdout(&db).trim().is_empty(),
        "a refused TTY entry must produce no output"
    );

    let redis = daemon.cli(&repo, &["redis"]);
    assert!(!redis.status.success());
    assert!(stderr(&redis).contains("needs an interactive terminal"));
    assert!(!stderr(&redis).contains("ServiceNotRunning"));
}

#[test]
fn auth_login_never_echoes_the_pairing_handle() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);
    let handle = "ab".repeat(32);
    let url = format!("http://127.0.0.1:1/pair#handle={handle}");

    let stdin_login = daemon.cli_with_stdin(&repo, &["auth", "login", "-"], &format!("{url}\n"));
    assert!(
        !stdin_login.status.success(),
        "an unreachable pairing endpoint must fail closed offline"
    );
    let combined = format!("{}{}", stdout(&stdin_login), stderr(&stdin_login));
    assert!(
        daemon.browser_marker.is_file(),
        "the injected BROWSER must open the pairing page"
    );
    assert!(
        fs::read_to_string(&daemon.browser_marker)
            .expect("browser marker")
            .is_empty(),
        "the browser stub must prove the opener ran without persisting the live URL"
    );
    assert!(
        !combined.contains(&handle),
        "the pairing handle must never be echoed: {combined}"
    );
    assert!(
        !combined.contains("#handle=ab"),
        "the fragment handle must never be rendered: {combined}"
    );

    let arg_login = daemon.cli(
        &repo,
        &[
            "auth",
            "login",
            "https://127.0.0.1:1/pair#handle=not-a-handle",
        ],
    );
    assert!(
        !arg_login.status.success(),
        "a malformed handle must fail closed"
    );
    assert!(
        !stderr(&arg_login).contains("not-a-handle"),
        "a rejected handle must not be echoed: {}",
        stderr(&arg_login)
    );
}

#[test]
fn support_json_lists_required_evidence_for_every_integration() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);

    let output = daemon.cli(&repo, &["--json", "support"]);
    assert_success(&output, "support --json");
    let rows: Value = serde_json::from_str(&stdout(&output)).expect("support json");
    let rows = rows.as_array().expect("support array");
    assert!(!rows.is_empty(), "the support registry must not be empty");
    for row in rows {
        assert!(row["id"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(row["category"].as_str().is_some());
        assert!(row["tier"].as_str().is_some());
        let required = row["required_evidence"]
            .as_array()
            .unwrap_or_else(|| panic!("missing required_evidence for {}", row["id"]));
        assert!(
            !required.is_empty(),
            "every integration must list required evidence: {}",
            row["id"]
        );
        assert!(required.iter().all(|field| field.as_str().is_some()));
    }

    let human = daemon.cli(&repo, &["support"]);
    assert_success(&human, "support");
    assert!(
        stdout(&human).contains("INTEGRATION") && stdout(&human).contains("TIER"),
        "the human report must render the matrix: {}",
        stdout(&human)
    );
}
