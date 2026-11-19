//! Protected `git-remote-slipfork` integration test (`ADR-0005` §3, `M2.17`).
//!
//! The real `git-remote-slipfork` binary is put on `PATH` and driven by a real `git fetch`: Git
//! spawns the helper, negotiates the documented line protocol, and the helper resolves both the
//! committed fork branch and the synthesized dirty view through the daemon-owned
//! `VcsWorkspace` (`DaemonControl::workspace_refs`), never by inspecting local Git refs itself.
//!
//! The refusal case kills `slipforkd` and proves the helper advertises nothing: a local
//! `refs/heads/feat-x` is not enough to serve `slipfork://feat-x`.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

pub mod support;

use support::{Daemon, disk_tempdir, repo_with_manifest, stderr, stdout};

const SNOW_YML: &str = r#"
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
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

fn run_git(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
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
        .expect("spawn git")
}

/// A `PATH` directory whose `git-remote-slipfork` is the just-built binary under test.
fn helper_path(scratch: &Path) -> String {
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("create helper bin dir");
    let helper = bin.join("git-remote-slipfork");
    let _ = fs::remove_file(&helper);
    symlink(env!("CARGO_BIN_EXE_git-remote-slipfork"), &helper).expect("symlink helper");
    let mut permissions = fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o755);
    let _ = fs::set_permissions(&helper, permissions);
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").expect("PATH is set")
    )
}

fn git_with_helper(repo: &Path, daemon: &Daemon, args: &[&str]) -> Output {
    Command::new("timeout")
        .args(["--signal=KILL", "120", "git", "-C"])
        .arg(repo)
        .args(args)
        .env(
            "PATH",
            helper_path(daemon.runtime_dir.parent().expect("runtime parent")),
        )
        .env(
            "XDG_CACHE_HOME",
            daemon.state_dir.parent().expect("state parent"),
        )
        .env("XDG_DATA_HOME", daemon.state_dir.join("xdg-data"))
        .output()
        .expect("run git")
}

#[test]
fn git_fetch_lists_and_fetches_fork_and_wip_refs_through_the_real_binary() {
    let tmp = disk_tempdir("v-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-git", SNOW_YML);
    let mut daemon = Daemon::start(scratch);

    let fork = daemon.cli(&repo, &["fork", "feat-x"]);
    assert!(
        fork.status.success(),
        "fork failed: {}\n{}",
        stdout(&fork),
        stderr(&fork)
    );
    let base = run_git(&repo, &["rev-parse", "--verify", "refs/heads/feat-x"]);
    assert!(base.status.success(), "the fork branch must exist");
    let base = String::from_utf8_lossy(&base.stdout).trim().to_owned();
    assert_eq!(base.len(), 40, "a real fork commit id");

    let remote = run_git(&repo, &["remote", "add", "slipfork", "slipfork://feat-x"]);
    assert!(remote.status.success());

    let fetch = git_with_helper(&repo, &daemon, &["fetch", "slipfork"]);
    assert!(
        fetch.status.success(),
        "git fetch slipfork failed: {}\n{}",
        stdout(&fetch),
        stderr(&fetch)
    );
    let fetched = run_git(
        &repo,
        &["rev-parse", "--verify", "refs/remotes/slipfork/feat-x"],
    );
    assert_eq!(
        String::from_utf8_lossy(&fetched.stdout).trim(),
        base,
        "the fetched branch must be the daemon-resolved fork commit"
    );

    let wip = git_with_helper(
        &repo,
        &daemon,
        &[
            "fetch",
            "slipfork",
            "+refs/slipfork/feat-x/wip:refs/slipfork/feat-x/wip",
        ],
    );
    assert!(
        wip.status.success(),
        "git fetch slipfork wip failed: {}\n{}",
        stdout(&wip),
        stderr(&wip)
    );
    let fetched_wip = run_git(
        &repo,
        &["rev-parse", "--verify", "refs/slipfork/feat-x/wip"],
    );
    assert_eq!(
        String::from_utf8_lossy(&fetched_wip.stdout).trim(),
        base,
        "a clean fork's dirty view must equal its base commit"
    );

    // The local Git objects are untouched, but the helper refuses without its daemon: a raw ref is
    // not a warrant to advertise a world that does not exist.
    daemon.shutdown();
    let refused = git_with_helper(&repo, &daemon, &["ls-remote", "slipfork"]);
    assert!(
        !refused.status.success(),
        "git ls-remote must fail closed when slipforkd is dead"
    );
    assert!(
        stdout(&refused).trim().is_empty(),
        "a dead daemon must never advertise refs: {}",
        stdout(&refused)
    );
    assert!(
        run_git(&repo, &["rev-parse", "--verify", "refs/heads/feat-x"])
            .status
            .success(),
        "the local fork ref is still present, proving the refusal came from the daemon path"
    );
}
