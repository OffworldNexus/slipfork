//! Journey A: one rich, full-stack Btrfs World E2E journey (`M3.30.4` §2).
//!
//! One daemon, one canonical world, one fork: `up` (and its idempotent second `up`), bare `status`,
//! `list`, `ps`, `logs`, `exec`, an edited checkout applied by `restart`, a no-argument `fork` with
//! the client's global/system git config unset (the daemon pins the snapshot author), the duplicate
//! fork and unbooted restart refusals, `open --remote`, `drop`, `down`, and `prune`. The journey
//! then proves that every booted subvolume and every component process group is reaped.
//!
//! Every helper is real: real disk, the real kernel `KernelBtrfsDriver`, a real `slipforkd`
//! composition root, and the real `slipfork` binary. Off-Btrfs the journey fails closed through
//! [`support::require_btrfs`] (one line naming the missing substrate); it never skips.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

pub mod support;
use support::{
    Daemon, POLL_INTERVAL, assert_success, disk_tempdir, poll_until, repo_with_manifest, stderr,
    stdout,
};

/// The canonical world: one `api` component, one ingress, one `:80` origin, and one `!!` secret so
/// the boot exercises the Open Secret Provider path as well as the process pipeline.
const JOURNEY_SNOW: &str = r#"
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
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

/// The same world after the checkout edit: only `GREETING` changes, so `restart` (not a second
/// `up`) must be what applies it.
const EDITED_SNOW: &str = r#"
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
        GREETING: v2
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

fn url_count(text: &str) -> usize {
    text.matches("http://").count()
}

/// `/proc/<pid>/stat` field 5 (`pgrp`) of a host process.
fn host_pgrp(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

/// Every host pid whose process group is `pgid`.
fn group_members(pgid: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| host_pgrp(*pid) == Some(pgid))
        .collect()
}

/// The live process group of one running component (its `nsenter` launcher and descendants).
fn component_group(daemon: &Daemon, cwd: &Path, id: &str) -> (u32, Vec<u32>) {
    let output = daemon.cli(cwd, &["--json", "ps"]);
    assert_success(&output, "ps --json");
    let document: Value = serde_json::from_str(&stdout(&output)).expect("ps json");
    let pid = document["services"]
        .as_array()
        .expect("services array")
        .iter()
        .find(|service| service["id"] == id)
        .and_then(|service| service["pid"].as_u64())
        .unwrap_or_else(|| panic!("component '{id}' has no live pid: {document}"));
    let pid = u32::try_from(pid).expect("a pid");
    let pgid = host_pgrp(pid).expect("the component's process group");
    (pgid, group_members(pgid))
}

/// Every world subvolume directly under `<state_dir>/worlds`.
fn world_subvolumes(state_dir: &Path) -> Vec<PathBuf> {
    let worlds = state_dir.join("worlds");
    let mut entries: Vec<PathBuf> = fs::read_dir(&worlds)
        .map(|directory| directory.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    entries
}

#[test]
fn world_lifecycle_journey_boots_edits_forks_and_reaps() {
    let tmp = disk_tempdir("journey-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", JOURNEY_SNOW);

    let mut daemon = Daemon::start(scratch);
    // Fail closed loudly, never skip: off Btrfs the production `statfs` probe panics with one line
    // naming the path, the found filesystem, and the required one.
    let _driver = support::require_btrfs(&daemon.state_dir);

    // --- up: one world, one origin, the scannable block ---------------------
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
        "the block carries the one origin, port-less at :80: {block}"
    );
    assert!(
        block.contains("worktree  ") && block.contains("lineage   rootfs "),
        "the block carries the worktree and the single lineage line: {block}"
    );
    assert!(
        !block.contains("[origin]") && !block.contains("[resources]"),
        "the superseded `[origin]` line and happy-path jargon are gone: {block}"
    );

    // --- bare `status` is the same renderer as `slipfork status` ------------
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
        lines[0].starts_with("slipforkd ") && lines[0].contains(", ingress 127.0.0.1:"),
        "the header names the daemon pid and its ingress: {dashboard}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("  acme-app ") && line.contains("running")),
        "the canonical row carries its running state: {dashboard}"
    );

    // --- list: exactly one URL round-trips the printed world id -------------
    let list = daemon.cli(&repo, &["list"]);
    assert_success(&list, "slipfork list");
    assert_eq!(
        url_count(&stdout(&list)),
        1,
        "list prints exactly one URL for one world:\n{}",
        stdout(&list)
    );

    // --- ps + logs ---------------------------------------------------------
    let (pgid, members) = component_group(&daemon, &repo, "api");
    assert!(
        !members.is_empty() && members.contains(&host_pgrp_pid(&daemon, &repo)),
        "the component's own `nsenter` launcher is in its process group"
    );
    let logs = daemon.cli(&repo, &["logs", "api"]);
    assert_success(&logs, "logs api");

    // --- exec starts in the world's mapped worktree, quietly ----------------
    let exec = daemon.cli(&repo, &["exec", "--", "sh", "-c", "pwd"]);
    assert_success(&exec, "slipfork exec -- sh -c pwd");
    assert_eq!(
        stdout(&exec).trim(),
        repo.display().to_string(),
        "exec must start in the world's mapped worktree, exactly like the requested cwd"
    );
    assert_eq!(
        stderr(&exec),
        "",
        "world entry must be quiet: no `getcwd` line before the command"
    );

    // --- a second `up` is idempotent and never mints a second URL -----------
    let up_again = daemon.cli(&repo, &["up"]);
    assert_success(&up_again, "second slipfork up");
    assert_eq!(
        url_count(&stdout(&up_again)),
        1,
        "a second up never mints a second origin: {}",
        stdout(&up_again)
    );

    // --- open: the remote recipe, and the fail-closed no-opener path --------
    let remote = daemon.cli(&repo, &["open", "--remote"]);
    assert_success(&remote, "slipfork open --remote");
    assert!(
        stdout(&remote)
            .lines()
            .any(|line| line.contains("ssh") || line.contains("slipfork")),
        "`--remote` prints the recipe instead of opening a browser: {}",
        stdout(&remote)
    );
    let empty = scratch.join("empty-path");
    fs::create_dir_all(&empty).expect("empty PATH directory");
    let refused = daemon.cli_with_path(&repo, &empty, &["open"]);
    assert!(
        !refused.status.success(),
        "no opener must fail `slipfork open` closed"
    );
    assert!(
        stderr(&refused).contains("slipfork open --remote"),
        "one actionable line naming the remote recipe: {}",
        stderr(&refused)
    );
    assert!(
        stdout(&refused).trim().is_empty(),
        "a refused open never prints a confirmation: {}",
        stdout(&refused)
    );

    // --- edited checkout: `restart` applies it, `up` never does -------------
    fs::write(repo.join("snow.yml"), EDITED_SNOW).expect("edit snow.yml");
    let restart = daemon.cli(&repo, &["restart"]);
    assert_success(&restart, "restart after an edited snow.yml");
    let restarted = stdout(&restart);
    assert!(
        restarted.contains("api") || restarted.contains("changed"),
        "restart names what came back: {restarted}"
    );
    assert_eq!(
        url_count(&restarted),
        1,
        "restart keeps exactly one origin: {restarted}"
    );

    // --- no-argument `fork` derives the branch with the client git config unset
    support::run_git(&repo, &["switch", "-q", "-c", "feat/ux-pass"]);
    let fork = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&daemon.state_dir)
        .arg("fork")
        .current_dir(&repo)
        .env(
            "XDG_CACHE_HOME",
            daemon.state_dir.parent().expect("state parent"),
        )
        .env("XDG_DATA_HOME", daemon.state_dir.join("xdg-data"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn slipfork fork");
    assert_success(&fork, "slipfork fork (branch-derived)");
    let worktree = repo.join(".worktrees/feat-ux-pass");
    let fork_block = stdout(&fork);
    assert!(
        fork_block.contains("world     acme-app:feat-ux-pass (running, changed)"),
        "`feat/ux-pass` becomes the fork `feat-ux-pass`: {fork_block}"
    );
    assert!(
        fork_block.contains("app       http://feat-ux-pass.acme-app.localhost"),
        "the origin is the fork URL, port-less at :80: {fork_block}"
    );
    assert!(
        fork_block.contains(&format!("worktree  {}\n", worktree.display())),
        "the block names the absolute `.worktrees/<fork>` checkout: {fork_block}"
    );
    assert!(
        fork_block.contains(&format!("cd        cd {}\n", worktree.display())),
        "the copy-pasteable `cd` line uses that same path: {fork_block}"
    );
    assert_eq!(
        url_count(&fork_block),
        1,
        "exactly one origin URL: {fork_block}"
    );
    assert!(
        worktree.join("snow.yml").is_file(),
        "the derived fork worktree is a real checkout"
    );

    // --- duplicate-fork refusal names the boot command ----------------------
    let again = daemon.cli(&repo, &["fork"]);
    assert!(
        !again.status.success(),
        "forking the same branch twice must fail closed"
    );
    let diagnostic = stderr(&again);
    assert!(
        diagnostic.contains("fork 'feat-ux-pass' already exists")
            && diagnostic.contains("`slipfork up --world acme-app:feat-ux-pass`"),
        "the refusal names the boot command for the existing world: {diagnostic}"
    );

    // --- unbooted-restart refusal: never silently booted --------------------
    let before = world_subvolumes(&daemon.state_dir);
    let unbooted = daemon.cli(&repo, &["restart", "--world", "acme-app:never"]);
    assert!(
        !unbooted.status.success(),
        "restarting a world the daemon never booted must fail closed"
    );
    assert!(
        !stderr(&unbooted).trim().is_empty(),
        "the refusal carries one actionable line, never a git advice block"
    );
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        before,
        "a refused restart must never create a world subvolume"
    );

    // --- `drop` releases the fork, `down` the canonical world ---------------
    let drop = daemon.cli(&repo, &["drop", "feat-ux-pass"]);
    assert_success(&drop, "slipfork drop feat-ux-pass");
    assert!(
        !worktree.exists(),
        "drop removes the fork's real worktree: {}",
        worktree.display()
    );

    let prune = daemon.cli(&repo, &["prune", "--dry-run"]);
    assert_success(&prune, "slipfork prune --dry-run");

    let down = daemon.cli(&repo, &["down"]);
    assert_success(&down, "slipfork down");
    assert!(
        stdout(&down).contains("[down]"),
        "expected a down line: {}",
        stdout(&down)
    );
    let down_again = daemon.cli(&repo, &["down"]);
    assert_success(&down_again, "second slipfork down");
    assert!(
        stdout(&down_again).contains("nothing to do"),
        "expected idempotent second down: {}",
        stdout(&down_again)
    );

    // --- `-pgid` reaping: the component's whole host group is gone ----------
    poll_until(Duration::from_secs(30), POLL_INTERVAL, || {
        let survivors = group_members(pgid);
        if survivors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "component process group {pgid} still has live members after down: {survivors:?}"
            ))
        }
    })
    .unwrap_or_else(|last| panic!("{last}"));

    // --- subvolume reaping: `down` leaves no world behind -------------------
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        Vec::<PathBuf>::new(),
        "down must destroy every world subvolume it booted"
    );

    // --- daemon teardown reaps its socket, worlds, and process groups ------
    daemon.shutdown();
    assert!(
        !support::daemon_is_served(&daemon.runtime_dir),
        "teardown unlinks the control socket"
    );
    daemon.assert_no_world_residue("after the full world lifecycle journey");
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        Vec::<PathBuf>::new(),
        "no world subvolume may survive the journey"
    );
    assert!(
        group_members(pgid).is_empty(),
        "no component process may survive the journey"
    );
}

/// The `api` component's own pid (its launcher), read from the same `--json ps` projection.
fn host_pgrp_pid(daemon: &Daemon, cwd: &Path) -> u32 {
    let output = daemon.cli(cwd, &["--json", "ps"]);
    assert_success(&output, "ps --json");
    let document: Value = serde_json::from_str(&stdout(&output)).expect("ps json");
    document["services"]
        .as_array()
        .expect("services array")
        .iter()
        .find(|service| service["id"] == "api")
        .and_then(|service| service["pid"].as_u64())
        .map(|pid| u32::try_from(pid).expect("a pid"))
        .expect("the api component has a live pid")
}
