//! Protected critical tests for `slipfork-vcs` (`R17`): `RepoRootResolver` + `VcsWorkspace`
//! conformance for `GitWorktreeAdapter`, the D/F-safe `refs/heads/<fork>` +
//! `refs/slipfork/<fork>/wip` layout, the untouched live `.git/index`, and fail-closed paths.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use slipfork_vcs::{
    GitRepoRootResolver, GitWorktreeAdapter, MAX_WORKSPACE_CHANGES, NoProjectOverride, RepoRoot,
    RepoRootResolver, VcsError, VcsWorkspace, VcsWorkspaceFactory, WorkspaceRequest,
};
use slipfork_world::WorldError;
use support::{FailingOverride, FixedOverride, assert_vcs_workspace_conformance, request};

/// Environment variable carrying the fixture root into the missing-`git` child probe.
const MISSING_GIT_CHILD_ENV: &str = "SLIPFORK_VCS_MISSING_GIT_CHILD_ROOT";

/// Extract a failed `create_workspace` error (`Box<dyn VcsWorkspace>` is not `Debug`).
fn expect_create_error(result: Result<Box<dyn VcsWorkspace>, VcsError>, context: &str) -> VcsError {
    match result {
        Ok(_) => panic!("{context}: expected an error, got a workspace"),
        Err(error) => error,
    }
}

#[test]
fn git_conformance_matrix_satisfies_shared_suite() {
    assert_vcs_workspace_conformance(&GitWorktreeAdapter::new());
}

#[test]
fn dirty_fork_snapshot_reproduces_modified_deleted_untracked_view() {
    let fixture = support::dirty_fixture("acme-app");
    let index_before = fixture.index_stat();
    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-billing"))
        .expect("dirty fork");

    assert_eq!(
        fixture.rev("refs/heads/feat-billing"),
        fixture.base,
        "the base branch is untouched by the snapshot"
    );
    assert!(
        fixture.ref_exists("refs/heads/feat-billing")
            && fixture.ref_exists("refs/slipfork/feat-billing/wip"),
        "refs/heads/<fork> and refs/slipfork/<fork>/wip coexist"
    );
    let wip = fixture.rev("refs/slipfork/feat-billing/wip");
    assert_eq!(
        fixture.git_ok(&["rev-list", "--parents", "-n", "1", &wip]),
        format!("{wip} {}", fixture.base),
        "the snapshot commit descends directly from the base commit"
    );

    // `read-tree -u --reset <wip>` then `read-tree <base>` is what the adapter does; the resulting
    // working tree must reproduce the developer's M / D / ?? view exactly.
    let status = support::git_stdout_in(workspace.path(), &["status", "--porcelain=v1"]);
    let mut lines: Vec<&str> = status.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [" D deleted.txt", " M tracked.txt", "?? untracked.txt"]
    );

    let changes = workspace.changes().expect("dirty changes");
    assert!(!changes.truncated);
    let mut tracked: Vec<String> = changes
        .tracked
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    tracked.sort();
    assert_eq!(tracked, ["deleted.txt", "tracked.txt"]);
    assert_eq!(changes.untracked, vec![PathBuf::from("untracked.txt")]);

    assert_eq!(
        fixture.index_stat(),
        index_before,
        "synthesizing the dirty view never touches the live .git/index"
    );
    assert!(
        fixture.scratch_index("feat-billing").exists(),
        "the snapshot is built through .slipfork/git/index-<fork>"
    );
}

#[test]
fn index_untouched_by_dirty_fork_snapshot() {
    let fixture = support::dirty_fixture("acme-app");
    let before = fixture.index_stat();
    assert_ne!(before.0, 0, "the live index has a real inode");

    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-index"))
        .expect("dirty fork");
    assert_eq!(
        fixture.index_stat(),
        before,
        "live .git/index inode + mtime must be unchanged across fork creation"
    );

    workspace.forget().expect("forget");
    assert_eq!(
        fixture.index_stat(),
        before,
        "teardown must not rewrite the live index either"
    );
}

#[test]
fn changes_truncates_large_dirty_views_at_the_port_limit() {
    let fixture = support::fixture("acme-app");
    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-many"))
        .expect("clean fork");
    for index in 0..=MAX_WORKSPACE_CHANGES {
        fs::write(
            workspace.path().join(format!("untracked-{index:04}.txt")),
            "x\n",
        )
        .expect("write untracked file inside the fork");
    }
    let changes = workspace.changes().expect("large dirty view is readable");
    assert!(changes.truncated, "truncated must flag the capped view");
    assert_eq!(changes.untracked.len(), MAX_WORKSPACE_CHANGES);
    assert!(changes.tracked.is_empty());
}

#[test]
fn fail_closed_fork_creation_refuses_existing_artifacts() {
    let adapter = GitWorktreeAdapter::new();

    let fixture = support::fixture("collide");
    let first = adapter
        .create_workspace(&request(&fixture.repo, "feat-a"))
        .expect("first fork");
    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, "feat-a")),
        "second fork for the same name is refused",
    );
    assert!(
        matches!(error, VcsError::WorkspaceExists { ref fork } if fork == "feat-a"),
        "{error}"
    );
    assert_eq!(
        fixture.rev("refs/heads/feat-a"),
        fixture.base,
        "the existing fork was not overwritten"
    );
    first.forget().expect("forget");

    let fixture = support::fixture("branch-only");
    fixture.git_ok(&["branch", "feat-b", &fixture.base]);
    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, "feat-b")),
        "an existing branch is never reused",
    );
    assert!(matches!(error, VcsError::WorkspaceExists { .. }), "{error}");
    assert!(!fixture.worktree("feat-b").exists());

    let fixture = support::fixture("wip-only");
    fixture.git_ok(&["update-ref", "refs/slipfork/feat-c/wip", &fixture.base]);
    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, "feat-c")),
        "an existing snapshot ref is never overwritten",
    );
    assert!(matches!(error, VcsError::WorkspaceExists { .. }), "{error}");

    let fixture = support::fixture("path-only");
    fs::create_dir_all(fixture.worktree("feat-d")).expect("create pre-existing worktree path");
    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, "feat-d")),
        "an existing .worktrees/<fork> directory is never reused",
    );
    assert!(matches!(error, VcsError::WorkspaceExists { .. }), "{error}");

    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, "../escape")),
        "fork names cannot escape .worktrees/",
    );
    assert!(
        matches!(error, VcsError::World(WorldError::InvalidForkName { .. })),
        "{error}"
    );
    assert!(
        !fixture
            .root()
            .parent()
            .expect("parent")
            .join("escape")
            .exists()
    );
}

#[test]
fn fail_closed_repository_and_override_errors_are_actionable() {
    let scratch = support::disk_tempdir("resolution");
    let outside = scratch.path().join("not-a-repo");
    fs::create_dir_all(&outside).expect("create non-repository directory");
    let outside = fs::canonicalize(&outside).expect("canonicalize non-repository directory");
    let resolver = GitRepoRootResolver::new(Arc::new(NoProjectOverride));
    let error = resolver
        .resolve(&outside)
        .expect_err("outside any repository fails closed");
    assert!(
        matches!(error, VcsError::NotARepository { ref start } if start == &outside),
        "{error}"
    );

    let bare = scratch.path().join("bare-origin.git");
    support::git_in(
        scratch.path(),
        &["init", "--bare", "-q", &bare.to_string_lossy()],
    );
    let error = resolver
        .resolve(&bare)
        .expect_err("bare repositories fail closed");
    assert!(
        matches!(
            error,
            VcsError::UnsupportedTopology {
                topology: "bare repository",
                ..
            }
        ),
        "{error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("git clone") && message.contains("slipfork.toml"),
        "bare refusal points at git clone + the project override, got: {message}"
    );

    let fixture = support::fixture("failing-override");
    let error = GitRepoRootResolver::new(Arc::new(FailingOverride))
        .resolve(fixture.root())
        .expect_err("override errors are propagated, never swallowed");
    assert!(
        matches!(error, VcsError::Io { ref message, .. } if message == "slipfork.toml is unreadable"),
        "{error}"
    );
}

#[test]
fn fail_closed_missing_git_binary_is_reported() {
    if std::env::var_os(MISSING_GIT_CHILD_ENV).is_some() {
        return;
    }
    let fixture = support::fixture("missing-git");
    let empty_path = support::disk_tempdir("empty-path");
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            "fail_closed_child_probe_without_git_on_path",
            "--nocapture",
        ])
        .env("PATH", empty_path.path())
        .env(MISSING_GIT_CHILD_ENV, fixture.root())
        .output()
        .expect("re-execute the test binary without git on PATH");
    assert!(
        output.status.success(),
        "child probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fail_closed_child_probe_without_git_on_path() {
    let Some(root) = std::env::var_os(MISSING_GIT_CHILD_ENV) else {
        return;
    };
    let error = GitRepoRootResolver::new(Arc::new(NoProjectOverride))
        .resolve(Path::new(&root))
        .expect_err("git is absent from PATH");
    assert_eq!(
        error,
        VcsError::ToolMissing {
            tool: "git".to_owned()
        }
    );
}

#[test]
fn repo_root_resolves_linked_worktrees_to_the_main_checkout() {
    let fixture = support::fixture("acme-app");
    let resolver = GitRepoRootResolver::new(Arc::new(NoProjectOverride));

    let from_subdirectory = resolver
        .resolve(&fixture.path("src"))
        .expect("subdirectory resolves");
    assert_eq!(from_subdirectory.vcs_root, fixture.repo.vcs_root);
    assert_eq!(from_subdirectory.worktree_root, fixture.repo.vcs_root);
    assert_eq!(from_subdirectory.project.as_str(), "acme-app");

    let linked = fixture.worktree("feat-link");
    fs::create_dir_all(linked.parent().expect("worktrees parent")).expect("create .worktrees");
    fixture.git_ok(&[
        "worktree",
        "add",
        "-q",
        &linked.to_string_lossy(),
        "-b",
        "feat-link",
        &fixture.base,
    ]);
    // A worktree-local `slipfork.toml` is never consulted (`ADR-0005` §3).
    fs::write(
        linked.join("slipfork.toml"),
        "project = \"rogue-worktree\"\n",
    )
    .expect("write worktree-local slipfork.toml");

    let from_linked = resolver
        .resolve(&linked)
        .expect("linked worktree resolves to the main checkout");
    assert_eq!(from_linked.vcs_root, fixture.repo.vcs_root);
    assert_eq!(
        from_linked.worktree_root,
        fs::canonicalize(&linked).expect("canonicalize linked worktree")
    );
    assert_eq!(
        from_linked.project.as_str(),
        "acme-app",
        "the slug comes from <vcs_root>, never the linked worktree basename or its slipfork.toml"
    );

    let (override_impl, seen) = FixedOverride::new("linked-override");
    let overridden = GitRepoRootResolver::new(Arc::new(override_impl))
        .resolve(&linked)
        .expect("linked worktree resolves with override");
    assert_eq!(overridden.project.as_str(), "linked-override");
    assert_eq!(
        seen.lock().expect("override lock").as_deref(),
        Some(fixture.repo.vcs_root.as_path()),
        "the override is read from <vcs_root>/slipfork.toml only"
    );
}

#[test]
fn fork_from_linked_worktree_snapshots_that_worktree() {
    let fixture = support::fixture("acme-app");
    let linked = fixture.worktree("feat-source");
    fs::create_dir_all(linked.parent().expect("worktrees parent")).expect("create .worktrees");
    fixture.git_ok(&[
        "worktree",
        "add",
        "-q",
        &linked.to_string_lossy(),
        "-b",
        "feat-source",
        &fixture.base,
    ]);
    fs::write(linked.join("tracked.txt"), "from-linked\n").expect("modify the linked worktree");

    // A request from `.worktrees/feat-source` carries that checkout as `worktree_root`, so the
    // snapshot must capture *its* dirty files while refs stay in the main checkout.
    let repo = RepoRoot {
        vcs_root: fixture.repo.vcs_root.clone(),
        worktree_root: fs::canonicalize(&linked).expect("canonicalize linked worktree"),
        project: fixture.repo.project.clone(),
    };
    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&WorkspaceRequest {
            repo,
            fork: "feat-child".to_owned(),
            base: None,
        })
        .expect("fork from a linked worktree");
    let wip = fixture.rev("refs/slipfork/feat-child/wip");
    assert_eq!(
        fixture.git_ok(&["show", &format!("{wip}:tracked.txt")]),
        "from-linked",
        "the snapshot comes from the requesting worktree, not the main checkout"
    );
    assert_eq!(
        fixture.rev("refs/heads/feat-child"),
        fixture.base,
        "the child fork keeps the linked worktree's HEAD as its base"
    );
    workspace.forget().expect("forget the child fork");
}

#[test]
fn repo_root_fails_closed_for_bare_submodule_and_separate_git_dir() {
    let scratch = support::disk_tempdir("topologies");

    // Git submodule: the slug may never be derived from `.git/modules/<id>`; an explicit
    // `project` override is the only rescue (`ADR-0005` §3).
    let upstream = scratch.path().join("upstream-mod");
    fs::create_dir_all(&upstream).expect("create upstream module");
    support::git_in(&upstream, &["init", "-q", "-b", "main"]);
    fs::write(upstream.join("lib.txt"), "x\n").expect("write upstream file");
    support::git_in(&upstream, &["add", "--", "lib.txt"]);
    support::git_in(&upstream, &["commit", "-q", "-m", "init"]);

    let parent = scratch.path().join("parent-monorepo");
    fs::create_dir_all(&parent).expect("create superproject");
    support::git_in(&parent, &["init", "-q", "-b", "main"]);
    support::git_in(
        &parent,
        &[
            "submodule",
            "add",
            "-q",
            &upstream.to_string_lossy(),
            "vendor/sub-pkg",
        ],
    );
    support::git_in(&parent, &["commit", "-q", "-m", "add submodule"]);
    let submodule = parent.join("vendor").join("sub-pkg");
    let submodule = fs::canonicalize(&submodule).expect("canonicalize submodule");

    let resolver = GitRepoRootResolver::new(Arc::new(NoProjectOverride));
    let error = resolver
        .resolve(&submodule)
        .expect_err("submodule without an override fails closed");
    assert!(
        matches!(
            error,
            VcsError::UnsupportedTopology {
                topology: "submodule",
                ..
            }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains(&submodule.join("slipfork.toml").display().to_string()),
        "submodule refusal names the concrete override file, got: {error}"
    );
    let (override_impl, _) = FixedOverride::new("sub-pkg");
    let rescued = GitRepoRootResolver::new(Arc::new(override_impl))
        .resolve(&submodule)
        .expect("an explicit override names the submodule");
    assert_eq!(rescued.vcs_root, submodule);
    assert_eq!(rescued.worktree_root, submodule);

    // `--separate-git-dir` checkout: always rejected, even when the admin store is named `.git`.
    let store = scratch.path().join("admin-store");
    let checkout = scratch.path().join("sep-checkout");
    support::git_in(
        scratch.path(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", store.display()),
            &checkout.to_string_lossy(),
        ],
    );
    let error = resolver
        .resolve(&checkout)
        .expect_err("separate git directory fails closed");
    assert!(
        matches!(
            error,
            VcsError::UnsupportedTopology {
                topology: "separate git directory",
                ..
            }
        ),
        "{error}"
    );

    let sneaky_parent = scratch.path().join("sneaky-holder");
    fs::create_dir_all(&sneaky_parent).expect("create sneaky holder");
    let sneaky = scratch.path().join("sneaky-sep");
    support::git_in(
        scratch.path(),
        &[
            "init",
            "-q",
            &format!(
                "--separate-git-dir={}",
                sneaky_parent.join(".git").display()
            ),
            &sneaky.to_string_lossy(),
        ],
    );
    let error = resolver
        .resolve(&sneaky)
        .expect_err("a /.git admin store is not a linked worktree");
    assert!(
        matches!(
            error,
            VcsError::UnsupportedTopology {
                topology: "separate git directory",
                ..
            }
        ),
        "{error}"
    );

    let (override_impl, _) = FixedOverride::new("sep-rescued");
    let rescued = GitRepoRootResolver::new(Arc::new(override_impl))
        .resolve(&checkout)
        .expect("an explicit override names the separate-git-dir checkout");
    assert_eq!(
        rescued.vcs_root,
        fs::canonicalize(&checkout).expect("canonicalize")
    );
    assert_eq!(rescued.project.as_str(), "sep-rescued");
}
