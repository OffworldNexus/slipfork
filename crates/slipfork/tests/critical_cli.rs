//! Protected end-to-end CLI + `slipforkd` critical tests (`H-010` #1, `ADR-0008` §4).
//!
//! These tests boot real `BtrfsEngine` worlds through the harness `slipforkd` composition root
//! (the real child, or the same in-process daemon graph over the real kernel driver), drive the
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
use slipfork::daemon_unit_install::{self, InstallRequest};
use slipfork_engine::btrfs::KernelBtrfsDriver;
use slipfork_ospp_testkit::{ReferenceOsppServer, ReferenceServerConfig};

pub mod support;
use support::{
    Daemon, POLL_INTERVAL, SHUTDOWN_DEADLINE, assert_success, poll_until, repo_with_manifest,
    stderr, stdout, wait_for_line,
};

#[path = "support/install_tool.rs"]
mod install_tool;
use install_tool::recording_tool;

/// The canonical `Forkfile` every test in this suite boots.
const FORKFILE: &str = r#"project acme-app

features {
    local
}

env app {
    BASE_URL = app.url
    SECRET_KEY = secret(SECRET_KEY) ?? random_string(32)
}

service api ./ {
    port 8000
    route /
    use env app
    run sleep 300
}
"#;

/// Disk-backed scratch directory with a short path (AF_UNIX `sun_path` budget), never `/tmp`.
fn disk_tempdir() -> tempfile::TempDir {
    support::disk_tempdir("c-")
}

/// A world whose single component prints a file copied into the image stage (`/marker.txt`), so a
/// rootfs rebuild is proven by the content the world actually booted, not only by its lineage.
const REBUILD_FORKFILE: &str = r#"project acme-rebuild

features {
    local
}

env app {
    BASE_URL = app.url
}

service api ./ {
    port 8000
    route /
    use env app
    run sh -c 'cat /marker.txt && exec sleep 300'
}
"#;

/// A manifest that declares no `project` statement: the daemon must derive the slug from the
/// checkout directory instead.
const NO_PROJECT_FORKFILE: &str = r#"features {
    local
}

env app {
    BASE_URL = "derived"
    SECRET_KEY = secret(SECRET_KEY) ?? random_string(32)
}

service api ./ {
    port 8000
    route /
    use env app
    run sleep 300
}
"#;

fn url_count(text: &str) -> usize {
    text.matches("http://").count()
}

// ---------------------------------------------------------------------------
// M3.28.10: zero-argument `slipfork fork` (branch-derived name, one-line refusals)
// ---------------------------------------------------------------------------

#[test]
fn a_forkfile_without_a_project_statement_derives_its_slug_from_the_directory() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    // The directory basename is the only project identity this checkout declares.
    let repo = repo_with_manifest(scratch, "derived-dir", NO_PROJECT_FORKFILE);
    let daemon = Daemon::start(scratch);

    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "up a repository with no `project` statement");
    let printed = stdout(&up);
    assert!(
        printed.contains("http://derived-dir.localhost"),
        "the world URL must derive from the directory slug: {printed}"
    );

    let status = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&status, "status --json after the derived-slug up");
    let document: Value = serde_json::from_str(&stdout(&status)).expect("status json");
    let worlds = document["worlds"].as_array().expect("worlds array");
    assert_eq!(worlds.len(), 1, "one world: {document}");
    assert_eq!(
        worlds[0]["world"], "derived-dir",
        "the daemon keys the world by the directory-derived slug"
    );

    let vault = daemon
        .state_dir
        .join("xdg-data/slipfork/vaults/derived-dir.json");
    assert!(
        vault.is_file(),
        "the daemon-side resolution keys the !! secret vault by the derived slug: {}",
        vault.display()
    );
}

#[test]
fn fork_and_drop_help_mark_the_name_optional() {
    let help = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .output()
            .expect("spawn slipfork --help");
        assert_success(&output, "slipfork --help");
        stdout(&output)
    };
    let fork = help(&["fork", "--help"]);
    assert!(fork.contains("[FORK]"), "the name is optional: {fork}");
    assert!(
        !fork.contains("<FORK>"),
        "no required-name usage line survives: {fork}"
    );
    assert!(
        fork.contains("Fork name (default: the current branch)"),
        "the help names the derivation: {fork}"
    );
    let drop = help(&["drop", "--help"]);
    assert!(
        drop.contains("[FORK]"),
        "the fork slot is optional and cwd-derived: {drop}"
    );
    assert!(
        drop.contains("cwd"),
        "the help names the cwd-derived default: {drop}"
    );
}

// ---------------------------------------------------------------------------
// M3.28.8: `slipfork restart` and the actionable `up`/`fork`/`restart` block
// ---------------------------------------------------------------------------

#[test]
fn restart_rebuilds_an_edited_rootfs_input_and_keeps_state_for_unchanged_inputs() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-rebuild", REBUILD_FORKFILE);
    let daemon = Daemon::start(scratch);

    // The app stage copies this marker and the component prints it, so a retained log proves which
    // rootfs content the world actually booted — not only what the lineage claims.
    fs::write(repo.join("marker.txt"), b"rootfs-v1\n").expect("write the rootfs marker");
    let dockerfile = fs::read_to_string(repo.join("Dockerfile")).expect("read Dockerfile");
    fs::write(
        repo.join("Dockerfile"),
        format!("{dockerfile}COPY marker.txt /marker.txt\n"),
    )
    .expect("copy the marker into the app stage");

    assert_success(&daemon.cli(&repo, &["up"]), "up before the rebuild");
    let first_ready = poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
        let logs = service_output(&daemon, &repo);
        if logs.contains("rootfs-v1") {
            Ok(())
        } else {
            Err(format!("component logs so far: {logs:?}"))
        }
    });
    assert!(
        first_ready.is_ok(),
        "the first boot serves the copied marker: {first_ready:?}"
    );
    let first_body = service_output(&daemon, &repo);
    assert!(
        !first_body.contains("rootfs-v2"),
        "the first boot must not serve content that does not exist yet: {first_body:?}"
    );
    let first = world_lineage(&daemon, &repo);
    let first_digest = first["rootfs"]["digest"]
        .as_str()
        .expect("the rootfs digest is recorded")
        .to_owned();
    assert_eq!(
        first["rootfs"]["cache"], "miss",
        "the first boot built the rootfs"
    );

    // A marker in the world's own subvolume: only a base-snapshot change may replace it.
    let subvolumes = canonical_subvolumes(&daemon.state_dir);
    assert_eq!(subvolumes.len(), 1, "one canonical world subvolume");
    let marker = subvolumes[0].join("state/keep-me.txt");
    fs::write(&marker, b"kept\n").expect("write a state marker");

    // Unchanged inputs: the same subvolume (and its state) is kept, and the lineage is a hit.
    assert_success(&daemon.cli(&repo, &["restart"]), "restart unchanged");
    let second = world_lineage(&daemon, &repo);
    assert_eq!(
        second["rootfs"]["cache"], "hit",
        "an unchanged tree must report the cache hit: {second}"
    );
    assert_eq!(
        second["rootfs"]["digest"], first_digest,
        "an unchanged tree keeps the same digest"
    );
    assert_eq!(
        canonical_subvolumes(&daemon.state_dir),
        subvolumes,
        "an unchanged tree keeps the same world subvolume"
    );
    assert!(
        marker.exists(),
        "world state must survive an unchanged restart"
    );

    // An edited copied stage input flips the content-addressed digest: the rootfs is rebuilt (miss)
    // and the component serves the new content, proving the reload booted the rebuild.
    fs::write(repo.join("marker.txt"), b"rootfs-v2\n").expect("edit the copied marker");
    let rebuilt = daemon.cli(&repo, &["restart"]);
    assert_success(&rebuilt, "restart after editing the Dockerfile input");
    let block = stdout(&rebuilt);
    assert!(
        block.contains("lineage   rootfs ") && block.contains(" rebuilt "),
        "an edited input must read `rebuilt`, not `cached`: {block}"
    );
    let second_ready = poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
        let logs = service_output(&daemon, &repo);
        if logs.contains("rootfs-v2") {
            Ok(())
        } else {
            Err(format!("component logs so far: {logs:?}"))
        }
    });
    assert!(
        second_ready.is_ok(),
        "the rebuilt world serves the edited marker: {second_ready:?}"
    );
    let second_body = service_output(&daemon, &repo);
    assert!(
        !second_body.contains("rootfs-v1"),
        "the restart must not serve the old rootfs content: {second_body:?}"
    );
    let third = world_lineage(&daemon, &repo);
    assert_eq!(
        third["rootfs"]["cache"], "miss",
        "the edited input must rebuild the rootfs: {third}"
    );
    assert_ne!(
        third["rootfs"]["digest"], first_digest,
        "the edited input must change the rootfs digest"
    );
    assert_eq!(
        canonical_subvolumes(&daemon.state_dir),
        subvolumes,
        "the base snapshot did not change, so the world subvolume (and state) is kept"
    );
    assert!(
        marker.exists(),
        "a rootfs-only edit keeps the backing-service state"
    );

    // And a further unchanged restart is a hit again: no unconditional rebuild.
    assert_success(&daemon.cli(&repo, &["restart"]), "restart unchanged again");
    assert_eq!(
        world_lineage(&daemon, &repo)["rootfs"]["cache"],
        "hit",
        "an unchanged tree must not rebuild the rootfs again"
    );
}

/// The repo world's `lineage` from the one `status --json` document (`worlds[0]`).
fn world_lineage(daemon: &Daemon, cwd: &Path) -> Value {
    let output = daemon.cli(cwd, &["--json", "status"]);
    assert_success(&output, "status --json");
    let document: Value = serde_json::from_str(&stdout(&output)).expect("status json");
    document["worlds"][0]["lineage"].clone()
}

/// The retained stdout of the repo's `api` component.
fn service_output(daemon: &Daemon, cwd: &Path) -> String {
    let output = daemon.cli(cwd, &["logs", "api"]);
    assert_success(&output, "logs api");
    stdout(&output)
}

/// The canonical world's one subvolume directory under the daemon's state root.
fn canonical_subvolumes(state_dir: &Path) -> Vec<PathBuf> {
    let mut subvolumes: Vec<PathBuf> = fs::read_dir(state_dir.join("worlds"))
        .expect("worlds directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("canonical-"))
        })
        .collect();
    subvolumes.sort();
    subvolumes
}

#[test]
fn home_unset_fails_closed_before_any_mutation() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);

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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
    let state_dir = scratch.join("no-daemon-state");
    let socket = state_dir.join("run").join("daemon.sock");
    let expected = format!(
        "slipfork: no slipforkd on {} — run `slipfork up`\n",
        socket.display()
    );

    // An empty fleet is not an error: bare `slipfork` and `status` read no daemon and exit `0`.
    for args in [vec![], vec!["status"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("--state-dir")
            .arg(&state_dir)
            .args(&args)
            .current_dir(&repo)
            .env("XDG_DATA_HOME", scratch.join("xdg-data"))
            .output()
            .expect("spawn the fleet view without a daemon");
        assert!(
            output.status.success(),
            "slipfork {args:?} is an empty fleet, not a failure"
        );
        assert_eq!(
            stdout(&output),
            "○ No worlds running\n",
            "slipfork {args:?} prints the one empty-fleet line"
        );
        assert_eq!(
            stderr(&output),
            "",
            "slipfork {args:?} needs no daemon to say nothing is running"
        );
        assert!(
            !state_dir.exists(),
            "a read-only fleet view must not create the state root"
        );
    }

    let json = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--json")
        .arg("status")
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("spawn `status --json` without a daemon");
    assert_success(&json, "status --json without a daemon");
    let document: Value = serde_json::from_str(&stdout(&json)).expect("status json");
    assert_eq!(document["ok"], true);
    assert!(
        document["ingress_port"].is_null(),
        "no daemon means no guessed ingress port: {document}"
    );
    assert_eq!(document["worlds"].as_array().map(Vec::len), Some(0));
    assert!(
        !state_dir.exists(),
        "a read-only `--json` read must not create the state root"
    );

    for args in [
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

#[test]
fn daemon_install_bytes_equal_a_real_daemon_unit_preview() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let state_dir = scratch.join("install-preview-state");
    // A real passwd row that is never the runner's own account: an install that ignored `--user`
    // would render `User=<runner>` and the byte comparison below would fail.
    assert!(
        slipfork::user::lookup_account("nobody").is_ok(),
        "this suite needs the real `nobody` passwd row to prove `--user` is honoured; add it to \
         the host account database and rerun"
    );
    let account = "nobody".to_owned();
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

    // The one concrete installer, over a real disk root and real tool programs: no recorded or
    // captured effect stands in for the files the install writes.
    let install_root = scratch.join("install-root");
    let unit_dir = install_root.join("systemd/system");
    let apparmor_dir = install_root.join("apparmor.d");
    std::fs::create_dir_all(&unit_dir).expect("create the system unit dir");
    std::fs::create_dir_all(&apparmor_dir).expect("create the apparmor dir");
    let log = install_root.join("tools.log");
    let installer = slipfork::daemon_unit_install::UnitInstaller::new(
        unit_dir,
        apparmor_dir,
        recording_tool(&install_root, "systemctl", &log),
        recording_tool(&install_root, "apparmor_parser", &log),
    );
    daemon_unit_install::install_streamed(
        &slipfork::user::UserIdentity::resolve().expect("the test runner's real OS identity"),
        &installer,
        daemon_unit_install::DAEMON_HANDOVER,
        &InstallRequest {
            is_root: true,
            explicit_user: Some(account),
            invoking_user: None,
            executable,
            bin_dirs: vec![bin_dir],
            forward_port: 7080,
            state_dir: Some(state_dir),
            xdg_cache_home: None,
            xdg_data_home: None,
        },
        &mut |_| {},
    )
    .expect("the concrete installer writes the preview's bytes");
    let paths = installer.paths();
    for (path, expected) in [
        (paths.socket, socket.as_slice()),
        (paths.service, service.as_slice()),
        (paths.apparmor, apparmor.as_slice()),
    ] {
        let written = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("{} must be written: {err}", path.display()));
        assert_eq!(
            written,
            expected,
            "{} must equal the real `daemon unit` stdout",
            path.display()
        );
        let mode = std::fs::metadata(&path)
            .expect("written file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "{} must be systemd-readable", path.display());
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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    let human = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(scratch.join("doctor-state"))
        .arg("doctor")
        .env("NO_COLOR", "1")
        .current_dir(&repo)
        .output()
        .expect("doctor");
    let text = stdout(&human);
    assert!(
        !text.contains('\x1b'),
        "doctor piped under NO_COLOR carries zero escape bytes: {text:?}"
    );
    assert!(
        text.lines()
            .next()
            .is_some_and(|line| line.starts_with('✓') || line.starts_with('✗')),
        "doctor leads with its verdict: {text}"
    );
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
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
    let state_dir = scratch.join("auto-state");
    let runtime_dir = state_dir.join("run");
    let daemon_log = runtime_dir.join("daemon.log");
    fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    // Capture the reap budget before the boot so the teardown proof never reads the host's load
    // average while it is already failing.
    let reap_budget = support::ReapBudget::current_host();

    let child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .args(["up"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-auto-data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn slipfork (auto-spawn)");
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
        // A refused boot never created an engine or a world, so only process residue is possible.
        if let Err(survivors) = support::reap_world_init_processes_within(scratch, reap_budget) {
            panic!("{survivors}\nafter the refused zero-setup `up` (non-Btrfs state root)");
        }
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

    // The CLI spawns the daemon in its own process group (`commands/control.rs`), so no signal to
    // the CLI's group can reach it: SIGTERM the pid the daemon reports about itself in `status`.
    let status = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&state_dir)
        .args(["--json", "status"])
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-auto-data"))
        .output()
        .expect("read the auto-spawned daemon status");
    assert_success(&status, "status after the auto-spawned up");
    let document: Value = serde_json::from_str(&stdout(&status)).expect("status json");
    let daemon_pid = document["pid"]
        .as_u64()
        .expect("the status document carries the daemon pid");
    let killed = Command::new("kill")
        .args(["-TERM", &daemon_pid.to_string()])
        .status()
        .expect("run kill");
    assert!(
        killed.success(),
        "SIGTERM the daemon pid {daemon_pid} the status document reports"
    );
    support::wait_daemon_teardown(&runtime_dir)
        .unwrap_or_else(|error| panic!("the auto-spawned slipforkd must stop accepting: {error}"));
    // The auto-spawned daemon was killed, so no engine `Drop` reclaimed the world it booted. The
    // proof names the child-daemon adapter directly (`KernelBtrfsDriver`, the one `slipforkd`'s
    // composition root builds), because this branch is only reached on a Btrfs state root and the
    // test has no harness `Daemon` handle to take it from.
    support::assert_no_world_residue(
        scratch,
        &state_dir,
        &KernelBtrfsDriver::new(),
        reap_budget,
        "after the auto-spawned slipforkd was SIGTERMed and stopped accepting",
    );
}

// ---------------------------------------------------------------------------
// M2.17: hook rescue UX, logs, shell, db/redis, forward, auth, support
// ---------------------------------------------------------------------------

const HOOK_FORKFILE: &str = r#"project acme-hook

features {
    local
}

env app {
    BASE_URL = app.url
}

service api ./ {
    port 8000
    route /
    use env app
    run sleep 300
    pre-deploy: sh -c 'echo HOOK-BOOM; echo HOOK-ERR >&2; exit 7'
}
"#;

const LOGS_FORKFILE: &str = r#"project acme-logs

features {
    local
}

env app {
    BASE_URL = app.url
}

service api ./ {
    port 8000
    route /
    use env app
    run sh -c 'while true; do echo tick-line; sleep 0.1; done'
}
"#;

const ATTRIBUTED_LOGS_FORKFILE: &str = r#"project acme-attributed-logs

features {
    local
}

env app {
    BASE_URL = app.url
}

service api ./ {
    port 8000
    route /
    use env app
    run sh -c 'while true; do echo api-line; sleep 0.1; done'
}

service worker ./ {
    use env app
    run sh -c 'while true; do echo worker-line; sleep 0.1; done'
}
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
    let repo = repo_with_manifest(scratch, "acme-hook", HOOK_FORKFILE);
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

    let listed = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&listed, "status after a failed hook");
    let document: Value = serde_json::from_str(&stdout(&listed)).expect("status json");
    let failed = document["worlds"]
        .as_array()
        .expect("worlds array")
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

    // The rescue commands re-run and inspect the failed world, and the same world is rescued by
    // `restart` once its own worktree's hook is fixed.
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

    // Rescue: the fork's own worktree is what a restart re-resolves, so fix the hook there.
    let manifest_path = repo.join(".worktrees/feat-bad/Forkfile");
    let manifest = fs::read_to_string(&manifest_path).expect("read the fork's Forkfile");
    fs::write(&manifest_path, manifest.replace("exit 7", "exit 0")).expect("fix the hook");
    let rescue = daemon.cli(&repo, &["restart", "--world", "acme-hook:feat-bad"]);
    assert_success(&rescue, "restart rescues the failed world");
    let block = stdout(&rescue);
    assert!(
        block.contains("[restarted] api\n"),
        "the rescue names the service it brought back: {block}"
    );
    assert!(
        block.contains("world     acme-hook:feat-bad (running, changed)"),
        "the rescued world is running: {block}"
    );
    assert!(
        block.contains("next      slipfork open --world acme-hook:feat-bad | "),
        "a world selected with --world gets paste-ready next commands: {block}"
    );
}

/// Run one `slipfork` invocation on a real PTY (`script(1)`), the way a developer's terminal does,
/// and send `stdin` to it. `require_tty` is a real `isatty(2)` probe, so no fake can stand in.
fn cli_on_a_pty(daemon: &Daemon, cwd: &Path, args: &[&str], stdin: &str) -> std::process::Output {
    let mut line = format!(
        "'{}' --state-dir '{}'",
        env!("CARGO_BIN_EXE_slipfork"),
        daemon.state_dir.display()
    );
    for arg in args {
        line.push_str(&format!(" '{arg}'"));
    }
    let mut child = Command::new("script")
        .args(["-qec", &line, "/dev/null"])
        .current_dir(cwd)
        .env(
            "XDG_CACHE_HOME",
            daemon.state_dir.parent().expect("state parent"),
        )
        .env("XDG_DATA_HOME", daemon.state_dir.join("xdg-data"))
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn script(1) for a real terminal");
    child
        .stdin
        .take()
        .expect("pty stdin")
        .write_all(stdin.as_bytes())
        .expect("write pty stdin");
    child.wait_with_output().expect("wait for script(1)")
}

/// The prompt `slipfork shell` hands over through `PS1`/`PROMPT` (`(slipfork:<world>/<service>)`).
const SHELL_PROMPT_PREFIX: &str = "(slipfork:";

#[test]
fn interactive_shell_hands_over_the_service_prompt_and_json_refuses() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up before shell");

    // A real PTY, plus a command that reads the exported prompt the way a login shell does.
    let shell = cli_on_a_pty(&daemon, &repo, &["shell"], "printenv PS1; exit\n");
    assert!(
        shell.status.success(),
        "interactive shell must hand over the terminal and exit cleanly: stdout={:?} stderr={:?}",
        stdout(&shell),
        stderr(&shell)
    );
    let out = stdout(&shell);
    assert!(
        out.contains(SHELL_PROMPT_PREFIX),
        "shell must export the world prompt: {out}"
    );
    assert!(
        out.contains("(slipfork:acme-app/api)"),
        "the prompt must name the selected world and service: {out}"
    );
    assert!(
        !out.contains("world rootfs is the app stage"),
        "the retired orientation banner must be gone: {out}"
    );

    // `--json` is a document mode: an interactive hand-over has no document, so it fails closed.
    let json = cli_on_a_pty(&daemon, &repo, &["--json", "shell"], "exit\n");
    assert!(
        !json.status.success(),
        "--json shell must fail closed instead of mixing a hand-over into a document: stdout={:?} stderr={:?}",
        stdout(&json),
        stderr(&json)
    );
    assert!(
        stdout(&json).contains("no --json document"),
        "the refusal must name the remedy: {}",
        stdout(&json)
    );

    // Non-interactive entry stays silent: `exec` shares the entry command, not the prompt.
    let exec = daemon.cli(&repo, &["exec", "--", "true"]);
    assert_success(&exec, "exec -- true");
    assert!(
        !stderr(&exec).contains("app stage"),
        "exec must stay silent: {}",
        stderr(&exec)
    );
}

#[test]
fn logs_follow_streams_new_component_output() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-logs", LOGS_FORKFILE);
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
fn logs_fan_out_attributes_every_line_to_its_component() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-attributed-logs", ATTRIBUTED_LOGS_FORKFILE);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up for attributed logs");

    let mut fan_out = String::new();
    let attributed = poll_until(Duration::from_secs(30), POLL_INTERVAL, || {
        fan_out = stdout(&daemon.cli(&repo, &["logs", "--lines", "40"]));
        if fan_out.contains("api: api-line") && fan_out.contains("worker: worker-line") {
            Ok(())
        } else {
            Err(fan_out.clone())
        }
    });
    assert!(
        attributed.is_ok(),
        "both components must stream attributed lines: {fan_out}"
    );
    for line in fan_out.lines().filter(|line| !line.is_empty()) {
        assert!(
            line.starts_with("api: ") || line.starts_with("worker: "),
            "every fan-out line names its source: {line}"
        );
    }

    // One explicitly named target stays byte-exact: the service's own text, no prefix.
    let single = daemon.cli(&repo, &["logs", "api", "--lines", "3"]);
    assert_success(&single, "logs api --lines 3");
    let single = stdout(&single);
    assert!(
        single.lines().any(|line| line == "api-line"),
        "a named target prints raw lines: {single}"
    );
    assert!(
        !single.contains("api: api-line"),
        "a named target is never prefixed: {single}"
    );

    // `--json` keeps one NDJSON document per chunk and carries the same structured source.
    let json = daemon.cli(&repo, &["logs", "--json", "--lines", "2", "api"]);
    assert_success(&json, "logs --json");
    let first = stdout(&json)
        .lines()
        .next()
        .expect("at least one NDJSON document")
        .to_owned();
    let document: Value = serde_json::from_str(&first).expect("one JSON document per chunk");
    assert_eq!(document["source"], "api");
    assert_eq!(document["stream"], "stdout");
}

#[test]
fn auth_prints_the_server_supplied_fragment_url_and_never_echoes_a_secret() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    let argv = match fs::read_to_string(&daemon.browser_argv) {
        Ok(argv) => argv,
        Err(error) => {
            let record_dir = daemon
                .browser_argv
                .parent()
                .expect("the browser argv record has a parent");
            let listing = match fs::read_dir(record_dir) {
                Ok(entries) => entries
                    .map(|entry| match entry {
                        Ok(entry) => entry.file_name().to_string_lossy().into_owned(),
                        Err(entry_error) => format!("<entry error: {entry_error}>"),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                Err(listing_error) => format!("<unreadable: {listing_error}>"),
            };
            let fixture = scratch.join("browser-bin").join("xdg-open");
            let executable = fs::metadata(&fixture)
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            let log_path = scratch.join("daemon.log");
            panic!(
                "the injected real opener did not record its argv ({error})\n\
                 record file: {}\n\
                 record dir ({}) listing: {listing}\n\
                 fixture {}: exists={} executable={executable}\n\
                 test process PATH={:?} DISPLAY={:?}\n\
                 daemon log ({}):\n{}",
                daemon.browser_argv.display(),
                record_dir.display(),
                fixture.display(),
                fixture.exists(),
                std::env::var("PATH").ok(),
                std::env::var("DISPLAY").ok(),
                log_path.display(),
                fs::read_to_string(&log_path)
                    .unwrap_or_else(|log_error| format!("<unreadable: {log_error}>")),
            )
        }
    };
    assert_eq!(
        argv.trim(),
        pairing_line.trim(),
        "the real opener receives exactly the printed fragment URL"
    );
    assert_eq!(
        argv.lines().count(),
        1,
        "the opener writes the URL it was handed and nothing else: {argv}"
    );
    assert_eq!(
        fs::read_dir(scratch.join("browser-bin"))
            .expect("the fixture directory")
            .count(),
        1,
        "the fixture writes its argv file outside the PATH directory, no marker file"
    );
    assert!(
        !printed.contains("auth-probe-secret"),
        "no command may echo a stored secret value: {printed}"
    );

    // The same journey on a host with no opener program: `auth` fails closed before it can hang
    // waiting for an approval page nobody can see.
    let empty = scratch.join("empty-path");
    fs::create_dir_all(&empty).expect("empty PATH directory");
    let headless_scratch = scratch.join("headless");
    fs::create_dir_all(&headless_scratch).expect("headless scratch");
    let headless_server = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let headless = Daemon::try_start_with_ospp_path(
        &headless_scratch,
        Some(&empty),
        headless_server.endpoint(),
    )
    .expect("a daemon on a host with no opener still serves");
    let refused = headless.cli(&repo, &["auth"]);
    assert!(
        !refused.status.success(),
        "a host with no opener fails `auth` closed instead of timing out"
    );
    assert!(
        stderr(&refused).contains("slipfork open --remote"),
        "one actionable line naming the remote recipe: {}",
        stderr(&refused)
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
        "! {}\n",
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
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
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
    assert!(
        !dashboard.contains('\x1b'),
        "a piped dashboard carries zero escape bytes: {dashboard:?}"
    );
    let no_color = daemon.cli_with_env(&repo, &["status"], "NO_COLOR", "1");
    assert_success(&no_color, "NO_COLOR status");
    assert_eq!(
        stdout(&no_color),
        dashboard,
        "NO_COLOR changes nothing on a non-terminal stdout"
    );
    let lines: Vec<&str> = dashboard.lines().collect();
    assert!(
        lines[0].starts_with("● acme-app  http://acme-app.localhost"),
        "the canonical row leads its own tree with the one origin: {dashboard}"
    );
    assert!(
        !dashboard.contains("slipforkd")
            && !dashboard.contains("next")
            && !dashboard.contains("lineage")
            && !dashboard.contains("--world"),
        "no daemon header, next spam, lineage, or selector spam: {dashboard}"
    );
    let fork_row = lines
        .iter()
        .position(|line| line.starts_with("└─ feat-ux  "))
        .unwrap_or_else(|| panic!("expected the fork under its project: {dashboard}"));
    assert!(
        lines[fork_row].contains("http://feat-ux.acme-app.localhost"),
        "the fork row carries its own origin: {dashboard}"
    );
    let verbose = daemon.cli(&repo, &["-v", "status"]);
    assert_success(&verbose, "status -v");
    assert!(
        stdout(&verbose).contains("lineage"),
        "the -v tree carries the lineage fact: {}",
        stdout(&verbose)
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
    let port = document["ingress_port"]
        .as_u64()
        .unwrap_or_else(|| panic!("ingress_port is always present: {document}"))
        as u16;
    assert!(
        port > 0,
        "the ephemeral daemon reports its real ingress port: {document}"
    );

    let worlds = document["worlds"]
        .as_array()
        .unwrap_or_else(|| panic!("worlds array: {document}"));
    assert_eq!(worlds.len(), 2, "canonical plus one fork");
    for world in worlds {
        assert_eq!(
            world_keys(world),
            vec![
                "lineage", "origin", "parent", "services", "state", "worktree", "world"
            ],
            "a status row carries exactly the documented keys: {world}"
        );
        assert_eq!(world["state"], "running");
        assert!(
            world["services"].is_array(),
            "every world carries its own service rows: {world}"
        );
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
fn status_with_zero_worlds_prints_one_line_and_exits_zero() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", FORKFILE);
    let daemon = Daemon::start(scratch);

    let bare = daemon.cli(&repo, &[]);
    assert_success(&bare, "bare slipfork with no worlds");
    assert_eq!(stdout(&bare), "○ No worlds running\n");

    let json_status = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&json_status, "status --json with no worlds");
    let document: Value = serde_json::from_str(&stdout(&json_status)).expect("status json");
    assert_eq!(document["ok"], true);
    let port = document["ingress_port"]
        .as_u64()
        .unwrap_or_else(|| panic!("a served daemon reports its real port: {document}"))
        as u16;
    let expected_stderr = if port == 80 {
        String::new()
    } else {
        ingress_hint_line(port)
    };
    assert_eq!(
        stderr(&bare),
        expected_stderr,
        "the empty fleet announces a non-:80 ingress exactly once, on stderr"
    );
    assert_eq!(
        document["worlds"].as_array().map(Vec::len),
        Some(0),
        "an empty fleet is an empty array, never a missing key: {document}"
    );

    // A fresh host with no daemon at all is the same empty fleet: bare `slipfork` and `status`
    // never start (or need) a daemon just to say nothing is running.
    let cold_state = scratch.join("cold-state");
    let cold = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&cold_state)
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("spawn bare slipfork on a cold host");
    assert_success(&cold, "bare slipfork on a cold host");
    assert_eq!(stdout(&cold), "○ No worlds running\n");
    assert_eq!(stderr(&cold), "");
    let cold_json = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&cold_state)
        .arg("--json")
        .arg("status")
        .current_dir(&repo)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .output()
        .expect("spawn status --json on a cold host");
    assert_success(&cold_json, "status --json on a cold host");
    let cold_document: Value = serde_json::from_str(&stdout(&cold_json)).expect("status json");
    assert_eq!(cold_document["ok"], true);
    assert!(
        cold_document["ingress_port"].is_null(),
        "no daemon means no guessed ingress port: {cold_document}"
    );
    assert_eq!(cold_document["worlds"].as_array().map(Vec::len), Some(0));
    assert!(
        !cold_state.exists(),
        "a read-only status never creates the state root"
    );
}
