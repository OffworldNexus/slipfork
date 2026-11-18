//! Protected end-to-end CLI + `slipforkd` critical tests (`H-010` #1, `ADR-0008` §4).
//!
//! These tests boot real `BtrfsEngine` worlds through the harness `slipforkd` composition root
//! (the real child on a Btrfs host, the in-process `TestTreeDriver` backend elsewhere), drive the
//! thin `slipfork` binary over the `0600` `<runtime_dir>/daemon.sock`, and assert the adversarial
//! contracts: fail-closed `HOME`, exactly one URL per world, idempotent `down`, secret overwrite
//! refusal, and non-template unit rendering.
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
use slipfork::daemon_unit_install::{self, InstallRequest, ManagedUnit, UnitInstaller};
use slipfork_engine::SystemClock;
use slipfork_engine::linux_cow::ProcessSignal;
use slipfork_ospp_testkit::{ReferenceOsppServer, ReferenceServerConfig};

pub mod support;
use support::{
    Daemon, POLL_INTERVAL, SHUTDOWN_DEADLINE, assert_success, poll_until, repo_with_manifest,
    stderr, stdout, wait_for_line,
};

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

/// A world whose `GREETING` env is the only thing the edit-and-apply test changes.
const RESTART_SNOW: &str = r#"
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
        GREETING: v1
"#;

/// A world whose single component is disabled in `local`: the pipeline starts nothing, so a
/// restart has no service to bring back (`changed: false`, `restarted: []`).
const IDLE_SNOW: &str = r#"
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

modes:
  local:
    components:
      api:
        disabled: true
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

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
    let block = stdout(&up);
    assert!(
        block.contains("world     acme-app (running, changed)"),
        "the block names the world, its state, and what changed: {block}"
    );
    assert!(
        block.contains("app       http://acme-app.localhost"),
        "the block carries the one origin: {block}"
    );
    assert!(
        block.contains("worktree  ") && block.contains("lineage   rootfs "),
        "the block carries the worktree and the single lineage line: {block}"
    );
    assert!(
        !block.contains("[origin]") && !block.contains("[resources]"),
        "the superseded `[origin]` line and happy-path jargon are gone: {block}"
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

// ---------------------------------------------------------------------------
// M3.28.8: `slipfork restart` and the actionable `up`/`fork`/`restart` block
// ---------------------------------------------------------------------------

/// The daemon's real ingress port, read from the one `status --json` document.
fn ingress_port(daemon: &Daemon, cwd: &Path) -> u16 {
    let output = daemon.cli(cwd, &["--json", "status"]);
    assert_success(&output, "status --json");
    let document: Value = serde_json::from_str(&stdout(&output)).expect("status json");
    u16::try_from(
        document["ingress"]["port"]
            .as_u64()
            .unwrap_or_else(|| panic!("ingress.port is always present: {document}")),
    )
    .expect("a u16 port")
}

/// The exact stderr the world verbs carry for `port` (`:80` means silence).
fn ingress_stderr(port: u16) -> String {
    if port == 80 {
        String::new()
    } else {
        ingress_hint_line(port)
    }
}

/// The world's `GREETING` from inside the running world (daemon-resolved env, not the host's).
fn world_greeting(daemon: &Daemon, cwd: &Path) -> String {
    let output = daemon.cli(cwd, &["exec", "--", "sh", "-c", "printf %s \"$GREETING\""]);
    assert_success(&output, "exec printf $GREETING");
    stdout(&output).trim().to_owned()
}

#[test]
fn restart_applies_an_edited_snow_file_and_names_what_came_back() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-restart", RESTART_SNOW);
    let daemon = Daemon::start(scratch);
    let port = ingress_port(&daemon, &repo);

    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "up before restart");
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v1",
        "the boot resolved the env"
    );
    assert_eq!(
        stderr(&up),
        ingress_stderr(port),
        "the block's only stderr is the one loud ingress line"
    );

    // `up` is idempotent: an edited `snow.yml` is applied by `restart`, never by a second `up`.
    let snow = fs::read_to_string(repo.join("snow.yml")).expect("read snow.yml");
    fs::write(
        repo.join("snow.yml"),
        snow.replace("GREETING: v1", "GREETING: v2"),
    )
    .expect("edit snow.yml");
    let again = daemon.cli(&repo, &["up"]);
    assert_success(&again, "second up");
    assert!(
        stdout(&again).contains(
            "world     acme-restart (running, unchanged — run `slipfork restart` to apply edits)"
        ),
        "an unchanged up names the verb that applies edits: {}",
        stdout(&again)
    );
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v1",
        "up alone never applies an edited snow.yml"
    );

    let restart = daemon.cli(&repo, &["restart"]);
    assert_success(&restart, "restart");
    let block = stdout(&restart);
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(
        lines[0], "[restarted] api",
        "the verdict is exactly the daemon's observed restarted ids: {block}"
    );
    assert_eq!(lines[1], "world     acme-restart (running, changed)");
    assert!(
        lines[2].starts_with("app       http://acme-restart.localhost"),
        "{block}"
    );
    assert!(lines[3].starts_with("worktree  "), "{block}");
    assert!(
        lines[4].starts_with("next      slipfork open | "),
        "{block}"
    );
    assert!(lines[5].starts_with("lineage   rootfs "), "{block}");
    assert_eq!(lines.len(), 6, "one verdict plus five block rows: {block}");
    assert_eq!(
        stderr(&restart),
        ingress_stderr(port),
        "exactly one ingress line on stderr, never on stdout"
    );
    assert!(
        !block.contains("ingress is :"),
        "the ingress line never reaches stdout"
    );
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v2",
        "restart applied the edited snow.yml env"
    );

    // `--json` is the daemon's own summary (one projection), with no human text and no stderr.
    let document = daemon.cli(&repo, &["--json", "restart"]);
    assert_success(&document, "restart --json");
    assert_eq!(
        stderr(&document),
        "",
        "--json never prints the ingress line"
    );
    let document: Value = serde_json::from_str(&stdout(&document)).expect("restart json");
    let mut keys: Vec<&str> = document
        .as_object()
        .unwrap_or_else(|| panic!("an object document: {document}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "changed",
            "lineage",
            "next",
            "ok",
            "origin",
            "resources",
            "restarted",
            "services",
            "state",
            "worktree",
            "world",
        ],
        "every documented key travels: {document}"
    );
    assert_eq!(document["ok"], true);
    assert_eq!(document["world"], "acme-restart");
    assert_eq!(document["state"], "running");
    assert_eq!(document["changed"], true);
    assert_eq!(document["restarted"][0], "api");
    assert!(
        document["origin"]
            .as_str()
            .is_some_and(|origin| origin.starts_with("http://acme-restart.localhost")),
        "`origin` is the typed origin's URL string: {document}"
    );
    for key in ["rootfs", "services", "seed", "base"] {
        assert!(
            document["lineage"].get(key).is_some(),
            "lineage.{key} travels as-is: {document}"
        );
    }
}

#[test]
fn restart_refuses_a_never_booted_world_and_rescues_a_failed_one() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-rescue-restart", HOOK_SNOW);
    let daemon = Daemon::start(scratch);

    // A world the daemon never booted is not silently booted by `restart`.
    let never = daemon.cli(&repo, &["restart"]);
    assert!(!never.status.success(), "restart must fail closed");
    assert!(!stdout(&never).contains("[restarted]"));
    let diagnostic = stderr(&never);
    assert!(
        diagnostic.contains("is not running") && diagnostic.contains("`slipfork up`"),
        "the refusal names `slipfork up`: {diagnostic}"
    );

    // A failed boot stays listed as `failed` (the rescue path's precondition).
    let fork = daemon.cli(&repo, &["fork", "feat-bad"]);
    assert!(!fork.status.success(), "the failing hook fails the fork");
    let listed = daemon.cli(&repo, &["--json", "list"]);
    assert_success(&listed, "list after the failed fork");
    let worlds: Value = serde_json::from_str(&stdout(&listed)).expect("list json");
    assert!(
        worlds
            .as_array()
            .expect("array")
            .iter()
            .any(|world| world["world"] == "acme-rescue-restart:feat-bad"
                && world["state"] == "failed"),
        "the failed fork stays listed: {worlds}"
    );

    // Rescue: the fork's own worktree is what a restart re-resolves, so fix the hook there.
    let snow_path = repo.join(".worktrees/feat-bad/snow.yml");
    let snow = fs::read_to_string(&snow_path).expect("read the fork's snow.yml");
    fs::write(&snow_path, snow.replace("exit 7", "exit 0")).expect("fix the hook");
    let rescue = daemon.cli(
        &repo,
        &["restart", "--world", "acme-rescue-restart:feat-bad"],
    );
    assert_success(&rescue, "restart rescues the failed world");
    let block = stdout(&rescue);
    assert!(
        block.contains("[restarted] api\n"),
        "the rescue names the service it brought back: {block}"
    );
    assert!(
        block.contains("world     acme-rescue-restart:feat-bad (running, changed)"),
        "the rescued world is running: {block}"
    );
    assert!(
        block.contains("next      slipfork open --world acme-rescue-restart:feat-bad | "),
        "a world selected with --world gets paste-ready next commands: {block}"
    );
}

#[test]
fn restart_reports_no_change_when_the_pipeline_starts_nothing() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-idle", IDLE_SNOW);
    let daemon = Daemon::start(scratch);

    assert_success(&daemon.cli(&repo, &["up"]), "up the idle world");

    let restart = daemon.cli(&repo, &["restart"]);
    assert_success(&restart, "restart an idle world");
    let block = stdout(&restart);
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(
        lines[0], "[no change]",
        "nothing came back, so the verdict says so: {block}"
    );
    assert_eq!(lines[1], "world     acme-idle (running, unchanged)");
    assert!(!block.contains("[restarted]"), "{block}");

    let document = daemon.cli(&repo, &["--json", "restart"]);
    assert_success(&document, "restart --json");
    let document: Value = serde_json::from_str(&stdout(&document)).expect("restart json");
    assert_eq!(document["changed"], false, "{document}");
    assert_eq!(
        document["restarted"].as_array().map(Vec::len),
        Some(0),
        "an empty id list, never a guess: {document}"
    );
}

#[test]
fn home_unset_fails_closed_before_any_mutation() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);

    // An explicit state root makes the "mutates nothing" assertion precise: the HOME guard must
    // fire before either the state directory or its `run/` runtime child is created.
    let state_dir = scratch.join("home-unset-state");
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
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
        !state_dir.exists(),
        "a HOME-unset invocation must not create state or runtime directories"
    );
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

    // The roots the command really resolves: the explicit layout plus the XDG defaults it would
    // otherwise fall back to. Both are watched below, so "daemon unit writes no files" can fail.
    let layout = support::DaemonLayout::under(scratch);
    let xdg_cache_home = scratch.join("xdg-cache");
    let xdg_data_home = scratch.join("xdg-data");
    let unit = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&layout.state_dir)
            .args(args)
            .current_dir(&repo)
            .env("XDG_CACHE_HOME", &xdg_cache_home)
            .env("XDG_DATA_HOME", &xdg_data_home)
            .output()
            .expect("daemon unit invocation")
    };

    let service = unit(&["daemon", "unit", "--only", "service"]);
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

    let socket = unit(&["daemon", "unit", "--only", "socket"]);
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

    let collision = unit(&["daemon", "unit", "--only", "socket", "--forward-port", "80"]);
    assert!(
        !collision.status.success(),
        "a forward port colliding with :80 must fail closed"
    );
    assert!(
        stderr(&collision).contains("collides"),
        "expected the unit port-collision diagnostic, got: {}",
        stderr(&collision)
    );

    // Rendering units is a pure stdout operation: neither the explicit layout roots nor the
    // default XDG roots the command would otherwise resolve may be created.
    for path in [
        &layout.state_dir,
        &layout.runtime_dir,
        &xdg_cache_home.join("slipfork"),
        &xdg_data_home,
    ] {
        assert!(
            !path.exists(),
            "daemon unit must not create {}",
            path.display()
        );
    }
}

#[test]
fn read_verbs_never_spawn_a_daemon_and_fail_closed_in_one_line() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let state_dir = scratch.join("no-daemon-state");
    let socket = state_dir.join("run").join("daemon.sock");
    let expected = format!(
        "slipfork: no slipforkd on {} — run `slipfork up`\n",
        socket.display()
    );

    for args in [
        vec![],
        vec!["status"],
        vec!["list"],
        vec!["down"],
        vec!["logs"],
        vec!["prune", "--dry-run"],
        vec!["secret", "list"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&state_dir)
            .args(&args)
            .current_dir(&repo)
            .env("XDG_DATA_HOME", scratch.join("xdg-data"))
            .output()
            .expect("spawn a read verb without a daemon");
        assert!(
            !output.status.success(),
            "slipfork {args:?} must fail closed without a daemon"
        );
        assert!(
            stdout(&output).is_empty(),
            "a refused read verb writes nothing to stdout: {}",
            stdout(&output)
        );
        assert_eq!(
            stderr(&output),
            expected,
            "slipfork {args:?} must print exactly the one no-daemon line"
        );
        assert!(
            !state_dir.exists(),
            "a refused read verb must not create the state root"
        );
    }
}

/// Recording [`UnitInstaller`] that keeps every rendered file for byte comparison.
#[derive(Default)]
struct CapturedInstaller {
    files: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Vec<u8>>>,
}

impl UnitInstaller for CapturedInstaller {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
        Ok(self.files.lock().expect("files").get(path).cloned())
    }

    fn write_file_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), String> {
        self.files
            .lock()
            .expect("files")
            .insert(path.to_path_buf(), contents.to_vec());
        Ok(())
    }

    fn daemon_reload(&self) -> Result<(), String> {
        Ok(())
    }

    fn apparmor_replace(&self, _profile_path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn stop_units(&self, _units: &[ManagedUnit]) -> Result<(), String> {
        Ok(())
    }

    fn is_served(&self, _runtime_dir: &Path) -> Result<bool, String> {
        Ok(false)
    }

    fn shutdown(&self, _runtime_dir: &Path) -> Result<(), String> {
        Ok(())
    }

    fn socket_holder(
        &self,
        _runtime_dir: &Path,
    ) -> Result<daemon_unit_install::SocketHolder, String> {
        Ok(daemon_unit_install::SocketHolder::Absent)
    }

    fn signal_proven(
        &self,
        _pidfd: &std::os::fd::OwnedFd,
        _signal: ProcessSignal,
    ) -> Result<(), String> {
        Ok(())
    }

    fn enable_now(&self, _units: &[ManagedUnit]) -> Result<(), String> {
        Ok(())
    }

    fn restart_service(&self) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn daemon_install_bytes_equal_a_real_daemon_unit_preview() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let state_dir = scratch.join("install-preview-state");
    let account = String::from_utf8(
        Command::new("id")
            .arg("-un")
            .output()
            .expect("id -un")
            .stdout,
    )
    .expect("utf8 account name");
    let account = account.trim().to_owned();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_slipforkd"));
    let bin_dir = executable
        .parent()
        .expect("daemon binary parent")
        .to_path_buf();
    let preview = |only: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&state_dir)
            .args([
                "daemon",
                "unit",
                "--user",
                &account,
                "--only",
                only,
                "--bin-dir",
                bin_dir.to_str().expect("utf8 bin dir"),
            ])
            .output()
            .expect("daemon unit preview");
        assert_success(&output, &format!("daemon unit --only {only}"));
        output.stdout
    };
    let socket = preview("socket");
    let service = preview("service");
    let apparmor = preview("apparmor");

    let installer = CapturedInstaller::default();
    daemon_unit_install::install(
        &slipfork::user::AccountDatabaseIdentity,
        &installer,
        &SystemClock,
        &InstallRequest {
            is_root: true,
            explicit_user: Some(account),
            executable,
            bin_dirs: vec![bin_dir],
            forward_port: 7080,
            state_dir: Some(state_dir),
            xdg_cache_home: None,
            xdg_data_home: None,
        },
    )
    .expect("the pure install renders the preview's bytes");
    let files = installer.files.lock().expect("files");
    for (path, expected) in [
        (daemon_unit_install::SOCKET_UNIT_PATH, socket.as_slice()),
        (daemon_unit_install::SERVICE_UNIT_PATH, service.as_slice()),
        (
            daemon_unit_install::APPARMOR_PROFILE_PATH,
            apparmor.as_slice(),
        ),
    ] {
        assert_eq!(
            files.get(Path::new(path)).map(Vec::as_slice),
            Some(expected),
            "{path} must equal the real `daemon unit` stdout"
        );
    }
}

#[test]
fn daemon_unit_refuses_an_apparmor_injecting_bin_dir_without_writing() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let state_dir = scratch.join("inject-state");
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .args([
            "daemon",
            "unit",
            "--only",
            "apparmor",
            "--bin-dir",
            "/tmp/a\nprofile x,",
        ])
        .output()
        .expect("daemon unit with an injecting --bin-dir");
    assert!(
        !output.status.success(),
        "an injecting --bin-dir must fail closed"
    );
    assert!(stdout(&output).is_empty(), "nothing is rendered");
    let err = stderr(&output);
    assert_eq!(err.lines().count(), 1, "exactly one line: {err}");
    assert!(
        err.starts_with("slipfork: invalid --bin-dir"),
        "the refusal names --bin-dir: {err}"
    );
    assert!(!state_dir.exists(), "the refusal writes nothing");
}

#[test]
fn daemon_unit_apparmor_lists_every_bin_dir_in_one_four_line_profile() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(scratch.join("unit-state"))
        .args([
            "daemon",
            "unit",
            "--only",
            "apparmor",
            "--bin-dir",
            "/usr/local/bin",
            "--bin-dir",
            "/tmp/ux/bin",
        ])
        .current_dir(&repo)
        .output()
        .expect("daemon unit --only apparmor");
    assert_success(&output, "daemon unit --only apparmor");

    let profile = stdout(&output);
    assert_eq!(profile.lines().count(), 4, "{profile}");
    assert_eq!(
        profile
            .lines()
            .filter(|line| line.contains("slipfork"))
            .count(),
        1,
        "the brace expression appears on exactly one line: {profile}"
    );
    assert_eq!(profile.matches("/{").count(), 1, "{profile}");
    assert!(!profile.contains("{{"), "no nested braces: {profile}");
    for clause in [
        "usr/local/bin/slipfork{,d,-world-init}",
        "tmp/ux/bin/slipfork{,d,-world-init}",
        "usr/bin/unshare",
        ".slipfork/bin/crun",
    ] {
        assert!(profile.contains(clause), "missing {clause}: {profile}");
    }
}

#[test]
fn doctor_json_carries_the_typed_ingress_document() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    // `doctor` is pre-daemon and advisory: a non-Btrfs host still emits the full document (and
    // exits 1), so only the ingress projection is asserted here.
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(scratch.join("doctor-state"))
        .args(["--json", "doctor"])
        .current_dir(&repo)
        .output()
        .expect("doctor --json");
    let value: Value = serde_json::from_str(&stdout(&output)).expect("doctor json");
    let ingress = value
        .get("ingress")
        .unwrap_or_else(|| panic!("doctor --json must carry `ingress`: {value}"));
    let status = ingress["status"]
        .as_str()
        .unwrap_or_else(|| panic!("ingress.status must be a string: {ingress}"));
    assert!(
        [
            "active",
            "active_other_listeners",
            "inactive",
            "not_installed"
        ]
        .contains(&status),
        "unexpected ingress status: {status}"
    );
    for key in ["port", "listen", "hint"] {
        assert!(
            ingress.get(key).is_some(),
            "ingress.{key} must always be present: {ingress}"
        );
    }
}

#[test]
fn zero_setup_auto_spawn_serves_up_without_a_prestarted_daemon() {
    use std::os::unix::process::CommandExt as _;

    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let state_dir = scratch.join("auto-state");
    let runtime_dir = state_dir.join("run");
    let daemon_log = runtime_dir.join("daemon.log");
    fs::create_dir_all(&runtime_dir).expect("create runtime dir");

    let child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
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
        support::assert_no_world_init_processes(
            scratch,
            "after the refused zero-setup `up` (non-Btrfs state root)",
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

    let termination = support::terminate_process_group(process_group);
    assert!(
        termination.term_delivered,
        "kill -TERM -{process_group} must succeed to prove the test reaped its daemon \
         (live members: {})",
        termination.members_alive
    );
    support::wait_daemon_teardown(&runtime_dir)
        .unwrap_or_else(|error| panic!("the auto-spawned slipforkd must stop accepting: {error}"));
    support::assert_no_world_init_processes(
        scratch,
        "after the auto-spawned slipforkd was SIGTERMed and stopped accepting",
    );
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
    poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
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
fn auth_prints_the_server_supplied_fragment_url_and_never_echoes_a_secret() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    // The daemon owns one boot-injected endpoint; the reference server mints the pairing URL and
    // its single-use handle, so the CLI must print exactly what the server supplied.
    let server = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let daemon = Daemon::start_with_ospp(scratch, server.endpoint());

    let stored = daemon.cli_with_stdin(
        &repo,
        &["secret", "set", "--force", "AUTH_PROBE"],
        "auth-probe-secret",
    );
    assert_success(&stored, "seed a local vault secret before pairing");

    let auth = daemon.cli(&repo, &["auth"]);
    assert_success(&auth, "slipfork auth");
    let printed = stdout(&auth);

    let pairing_line = printed
        .lines()
        .find(|line| line.contains("#handle="))
        .unwrap_or_else(|| panic!("auth must print the fragment URL: {printed}"));
    let expected_prefix = format!("http://{}/pair#handle=", server.dump_addr());
    assert!(
        pairing_line.starts_with(&expected_prefix),
        "the daemon must print the server-supplied URL, never one it computes: {pairing_line}"
    );
    let handle = pairing_line
        .trim()
        .strip_prefix(&expected_prefix)
        .expect("fragment handle");
    assert_eq!(
        handle.len(),
        64,
        "the fragment handle is the server's 32-byte hex value: {handle}"
    );
    assert!(
        handle.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the fragment handle must be hex: {handle}"
    );
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
        !printed.contains("auth-probe-secret"),
        "no command may echo a stored secret value: {printed}"
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

// ---------------------------------------------------------------------------
// M3.28.7: bare `slipfork` is the status dashboard (`docs/design/dx-0.1.0-ux.md` §3)
// ---------------------------------------------------------------------------

/// The one `:80` line stderr carries when the daemon answered on another ingress port.
///
/// Rendered by [`slipfork::status::ingress_hint`]; the test never re-types the format.
fn ingress_hint_line(port: u16) -> String {
    format!(
        "{}\n",
        slipfork::status::ingress_hint(port).expect("a non-:80 ingress is announced")
    )
}

/// One world object's `--json` key set: every key, and only the status document's keys.
fn world_keys(world: &Value) -> Vec<String> {
    let mut keys: Vec<String> = world
        .as_object()
        .unwrap_or_else(|| panic!("a world row is an object: {world}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn bare_slipfork_and_status_render_one_dashboard_and_json_stays_pure() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up the canonical world");
    assert_success(&daemon.cli(&repo, &["fork", "feat-ux"]), "fork feat-ux");

    // `--help` still lists every command, and `slipfork help` renders the same list.
    let help = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--help")
        .output()
        .expect("slipfork --help");
    assert_success(&help, "slipfork --help");
    let help_text = stdout(&help);
    assert!(
        help_text.contains("Commands:"),
        "the help keeps its Commands block: {help_text}"
    );
    assert!(
        help_text.lines().any(|line| line.starts_with("  status")),
        "`status` must be listed: {help_text}"
    );
    let help_subcommand = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("help")
        .output()
        .expect("slipfork help");
    assert_success(&help_subcommand, "slipfork help");
    assert!(
        stdout(&help_subcommand).contains("  status"),
        "`slipfork help` lists the same commands: {}",
        stdout(&help_subcommand)
    );

    // Bare `slipfork` is the exact same handler, so the dashboard is byte-identical.
    let bare = daemon.cli(&repo, &[]);
    assert_success(&bare, "bare slipfork");
    let explicit = daemon.cli(&repo, &["status"]);
    assert_success(&explicit, "slipfork status");
    assert_eq!(
        stdout(&bare),
        stdout(&explicit),
        "bare slipfork must run `slipfork status`, not a second renderer"
    );

    let dashboard = stdout(&bare);
    let lines: Vec<&str> = dashboard.lines().collect();
    assert!(
        lines[0].starts_with("slipforkd ") && lines[0].contains(", ingress 127.0.0.1:"),
        "the header names the daemon pid and its ingress: {dashboard}"
    );
    let canonical = lines
        .iter()
        .position(|line| line.starts_with("  acme-app "))
        .unwrap_or_else(|| panic!("expected a canonical row: {dashboard}"));
    assert!(
        lines[canonical].contains("running")
            && lines[canonical].contains("http://acme-app.localhost"),
        "the canonical row carries its state and origin: {dashboard}"
    );
    let label_column = lines[canonical + 1]
        .find("lineage")
        .unwrap_or_else(|| panic!("each world row is followed by its lineage line: {dashboard}"));
    assert!(
        lines[canonical + 1][..label_column].trim().is_empty(),
        "the lineage label sits in an indented label column: {dashboard}"
    );

    // Derive both sub-lines instead of re-typing the literal indent/hint text asserted once in
    // `slipfork::status`'s own tests.
    let sub_line = |row: usize, label: &str| -> String {
        lines[row]
            .strip_prefix(&" ".repeat(label_column))
            .and_then(|tail| tail.strip_prefix(label))
            .unwrap_or_else(|| panic!("expected a `{label}` sub-line for row {row}: {dashboard}"))
            .to_owned()
    };
    let canonical_next = sub_line(canonical + 2, "next      ");
    assert!(
        sub_line(canonical + 1, "lineage   ").starts_with("rootfs "),
        "the lineage line starts with the rootfs fact: {dashboard}"
    );
    assert!(
        !canonical_next.contains("--world"),
        "the canonical world's hint stays bare: {canonical_next}"
    );
    let implemented = slipfork::cli::implemented_commands();
    for hint in canonical_next.split(" | ") {
        let verb = hint
            .strip_prefix("slipfork ")
            .unwrap_or_else(|| panic!("a hint is a full `slipfork <verb>` command: {hint}"));
        assert!(
            implemented.contains(verb),
            "`{hint}` is hinted but is not a subcommand of this binary: {dashboard}"
        );
    }

    let fork_row = lines
        .iter()
        .position(|line| line.starts_with("    acme-app:feat-ux "))
        .unwrap_or_else(|| panic!("expected the fork indented under its project: {dashboard}"));
    assert!(
        fork_row > canonical,
        "the canonical world leads its own forks: {dashboard}"
    );
    assert!(
        lines[fork_row].contains("http://feat-ux.acme-app.localhost"),
        "the fork row carries its own origin: {dashboard}"
    );
    let expected_fork_next = canonical_next
        .split(" | ")
        .map(|hint| format!("{hint} --world acme-app:feat-ux"))
        .collect::<Vec<_>>()
        .join(" | ");
    assert_eq!(
        sub_line(fork_row + 2, "next      "),
        expected_fork_next,
        "a fork hint pastes straight back into --world: {dashboard}"
    );

    // `--json` is the same single read projected, with no human text on stdout.
    let json_status = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&json_status, "status --json");
    assert_eq!(
        stderr(&json_status),
        "",
        "--json never prints the human ingress line"
    );
    let bare_json = daemon.cli(&repo, &["--json"]);
    assert_success(&bare_json, "bare slipfork --json");
    assert_eq!(
        stdout(&bare_json),
        stdout(&json_status),
        "the bare --json document is the same one `status --json` emits"
    );

    let document: Value = serde_json::from_str(&stdout(&json_status)).expect("status json");
    assert_eq!(document["ok"], true);
    assert!(!stdout(&json_status).contains("slipforkd"));
    assert!(!stdout(&json_status).contains("lineage   "));
    assert!(document["pid"].as_u64().is_some_and(|pid| pid > 0));
    let port = document["ingress"]["port"]
        .as_u64()
        .unwrap_or_else(|| panic!("ingress.port is always present: {document}"))
        as u16;
    assert_eq!(
        document["ingress"]["addrs"]
            .as_array()
            .expect("addrs array")
            .len(),
        1,
        "the ephemeral daemon binds one ingress listener: {document}"
    );
    assert!(
        document["forward_port"].is_null(),
        "a daemon with no forward listener reports null: {document}"
    );

    let worlds = document["worlds"]
        .as_array()
        .unwrap_or_else(|| panic!("worlds array: {document}"));
    assert_eq!(worlds.len(), 2, "canonical plus one fork");
    for world in worlds {
        assert_eq!(
            world_keys(world),
            vec!["lineage", "origin", "parent", "state", "worktree", "world"],
            "a status row carries exactly the documented keys: {world}"
        );
        assert_eq!(world["state"], "running");
        assert_eq!(
            world["origin"]
                .as_str()
                .expect("origin string")
                .matches("http://")
                .count(),
            1,
            "every world carries exactly one origin: {world}"
        );
        for key in ["rootfs", "services", "seed", "base"] {
            assert!(
                world["lineage"].get(key).is_some(),
                "lineage.{key} is always present: {world}"
            );
        }
    }
    let canonical_json = worlds
        .iter()
        .find(|world| world["world"] == "acme-app")
        .expect("canonical row");
    let fork_json = worlds
        .iter()
        .find(|world| world["world"] == "acme-app:feat-ux")
        .expect("fork row");
    assert!(
        canonical_json["parent"].is_null(),
        "the canonical world has no parent: {canonical_json}"
    );
    assert_eq!(fork_json["parent"], "acme-app");

    // The loud `:80` line: exactly one stderr line in human mode, and it names the installer.
    let expected_stderr = if port == 80 {
        String::new()
    } else {
        ingress_hint_line(port)
    };
    assert_eq!(
        stderr(&bare),
        expected_stderr,
        "exactly one actionable ingress line on stderr (port {port})"
    );
    assert!(
        !stdout(&bare).contains("ingress is :"),
        "the ingress line never reaches stdout: {dashboard}"
    );
}

#[test]
fn status_with_zero_worlds_prints_one_line_plus_up() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_snow(scratch);
    let daemon = Daemon::start(scratch);

    let bare = daemon.cli(&repo, &[]);
    assert_success(&bare, "bare slipfork with no worlds");
    let dashboard = stdout(&bare);
    let lines: Vec<&str> = dashboard.lines().collect();
    assert_eq!(lines.len(), 2, "one daemon line plus one hint: {dashboard}");
    assert!(
        lines[0].starts_with("slipforkd ") && lines[0].contains(", ingress 127.0.0.1:"),
        "the daemon line stays first: {dashboard}"
    );
    assert_eq!(
        lines[1], "  no worlds — run `slipfork up` inside a project checkout",
        "a daemon with no world names the command that creates one: {dashboard}"
    );

    let json_status = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&json_status, "status --json with no worlds");
    let document: Value = serde_json::from_str(&stdout(&json_status)).expect("status json");
    assert_eq!(document["ok"], true);
    assert_eq!(
        document["worlds"].as_array().map(Vec::len),
        Some(0),
        "an empty fleet is an empty array, never a missing key: {document}"
    );
}
