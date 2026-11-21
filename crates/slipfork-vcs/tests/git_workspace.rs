//! Real `git` against a real repository on disk under `/var/tmp`: no mocks, no scripted stubs.
//!
//! The journey is the one the daemon performs: read the branch, dirty the checkout, clean it again,
//! switch to an existing branch, then create a new one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use slipfork_vcs::{Dirty, GitWorkspace, VcsError, VcsWorkspace};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("the vcs lab needs a real `git` on PATH");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A real repository on disk (`/var/tmp`, never RAM `/tmp`) with one commit on `main`.
fn repository() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("slipfork-vcs-")
        .tempdir_in("/var/tmp")
        .expect("the vcs lab root must be writable disk");
    let project = root.path().join("demo");
    fs::create_dir_all(&project).expect("create the checkout");
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.email", "test@slipfork.local"]);
    git(&project, &["config", "user.name", "Slipfork Test"]);
    fs::write(project.join("README.md"), "lab\n").expect("write the seed file");
    git(&project, &["add", "README.md"]);
    git(&project, &["commit", "-m", "seed"]);
    (root, project)
}

#[test]
fn branch_switch_and_dirtiness_track_a_real_checkout() {
    let (root, project) = repository();
    let workspace = GitWorkspace::new("git");

    assert_eq!(workspace.branch(&project).expect("branch"), "main");
    assert_eq!(workspace.dirtiness(&project).expect("clean"), Dirty::Clean);

    // An untracked file is dirty work; committing it makes the checkout clean again.
    fs::write(project.join("dirty.txt"), "bytes\n").expect("write the dirty marker");
    assert_eq!(workspace.dirtiness(&project).expect("dirty"), Dirty::Dirty);
    git(&project, &["add", "dirty.txt"]);
    git(&project, &["commit", "-m", "dirty"]);
    assert_eq!(workspace.dirtiness(&project).expect("clean"), Dirty::Clean);

    // An existing branch switches without creating a second one.
    git(&project, &["branch", "feat-existing"]);
    workspace
        .switch(&project, "feat-existing")
        .expect("switch existing");
    assert_eq!(workspace.branch(&project).expect("branch"), "feat-existing");

    // A branch the parent never had is created in the fork checkout.
    workspace.switch(&project, "feat-new").expect("switch new");
    assert_eq!(workspace.branch(&project).expect("branch"), "feat-new");

    // A checkout that never materialised is clean, so `drop` never refuses it.
    let absent = root.path().join("absent");
    assert_eq!(workspace.dirtiness(&absent).expect("absent"), Dirty::Clean);

    // A detached HEAD is a typed one-line refusal, never a `git` transcript.
    git(&project, &["checkout", "--detach", "HEAD"]);
    let error = workspace.branch(&project).expect_err("detached HEAD");
    assert!(matches!(error, VcsError::NoBranch { .. }), "{error:?}");
    let message = error.to_string();
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(message.contains("detached HEAD"), "{message}");
}
