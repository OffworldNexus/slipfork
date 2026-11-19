//! Protected critical tests for `slipfork-vcs` (`R17`): `RepoRootResolver` + `VcsWorkspace`
//! conformance for `GitWorktreeAdapter` and `JjWorkspaceAdapter`, the D/F-safe `refs/heads/<fork>` +
//! `refs/slipfork/<fork>/wip` layout, the untouched live `.git/index`, the branch probe, the pinned
//! snapshot identity, one-line VCS errors, the `git-remote-slipfork` line protocol end to end, and
//! the fail-closed paths.

mod support;

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use slipfork_vcs::remote_helper::{self, MAX_FRAME_BYTES, RemoteHelperError};
use slipfork_vcs::{
    GitRepoRootResolver, GitWorktreeAdapter, JjWorkspaceAdapter, MAX_WORKSPACE_CHANGES,
    NoProjectOverride, ProjectSlugOverride, RepoRoot, RepoRootResolver, VcsError, VcsWorkspace,
    VcsWorkspaceFactory, WorkspaceChanges, WorkspaceRefs, WorkspaceRequest, sanitize_fork_name,
};
use slipfork_world::{ProjectSlug, WorldError};
use support::{FixedOverride, expect_create_error, request};

/// `(inode, mtime seconds, mtime nanoseconds)` of the live `.git/index`.
///
/// The fork synthesizer must never open, rename, or lock this file, so these values are the
/// protected invariant for a dirty fork of a large working tree.
impl support::Fixture {
    fn index_stat(&self) -> (u64, i64, i64) {
        let index = self.root().join(".git").join("index");
        let metadata = fs::metadata(&index).expect("live .git/index exists");
        (metadata.ino(), metadata.mtime(), metadata.mtime_nsec())
    }
}

/// Create a named FIFO, so a test can relay Git's helper protocol into an in-process handler.
fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo {} failed", path.display());
}

/// [`VcsWorkspace`] double with fixed ref ids, for byte-exact protocol assertions.
///
/// `remote_helper` is pure over the port, so the protocol frames can be pinned exactly without a
/// repository; the adapters that back the port in production are covered by the conformance matrix.
struct RefWorkspace {
    base: String,
    wip: String,
}

impl RefWorkspace {
    fn new(base: &str, wip: &str) -> Self {
        Self {
            base: base.to_owned(),
            wip: wip.to_owned(),
        }
    }
}

impl VcsWorkspace for RefWorkspace {
    fn fork_name(&self) -> &str {
        "feat-billing"
    }

    fn path(&self) -> &Path {
        Path::new("/repo/.worktrees/feat-billing")
    }

    fn refs(&self) -> WorkspaceRefs {
        WorkspaceRefs {
            base: "refs/heads/feat-billing".to_owned(),
            wip: "refs/slipfork/feat-billing/wip".to_owned(),
        }
    }

    fn resolve(&self, reference: &str) -> Result<String, VcsError> {
        match reference {
            "refs/heads/feat-billing" => Ok(self.base.clone()),
            "refs/slipfork/feat-billing/wip" => Ok(self.wip.clone()),
            other => Err(VcsError::UnexpectedOutput {
                command: "resolve".to_owned(),
                message: format!("the double has no ref '{other}'"),
            }),
        }
    }

    fn changes(&self) -> Result<WorkspaceChanges, VcsError> {
        Ok(WorkspaceChanges::default())
    }

    fn forget(&self) -> Result<(), VcsError> {
        Ok(())
    }
}

/// [`ProjectSlugOverride`] double proving adapter errors are propagated verbatim.
struct FailingOverride;

impl ProjectSlugOverride for FailingOverride {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        Err(VcsError::Io {
            path: vcs_root.to_path_buf(),
            message: "slipfork.toml is unreadable".to_owned(),
        })
    }
}

/// Environment variable carrying the fixture root into the missing-`git` child probe.
const MISSING_GIT_CHILD_ENV: &str = "SLIPFORK_VCS_MISSING_GIT_CHILD_ROOT";
/// Environment variable carrying the scratch root into the missing-`jj` child probe.
const MISSING_JJ_CHILD_ENV: &str = "SLIPFORK_VCS_MISSING_JJ_CHILD_ROOT";

#[test]
fn git_current_branch_reports_none_when_detached() {
    // The branch case is covered by the shared matrix; here only the detached half, because a
    // detached `HEAD` is what makes zero-argument `fork` fail closed.
    let fixture = support::fixture("branch-probe");
    fixture.git_ok(&["checkout", "-q", "--detach", &fixture.base]);
    assert_eq!(
        GitWorktreeAdapter::new()
            .current_branch(&fixture.repo)
            .expect("a detached HEAD still answers the probe"),
        None,
        "a detached HEAD answers None so the caller fails closed with `slipfork fork <name>`"
    );
}

#[test]
fn vcs_failures_render_as_one_line() {
    // `git rev-parse` on an unknown base is a real non-zero exit; the owner must see one line, not
    // Git's multi-line advice block (`docs/design/dx-0.1.0-ux.md` §7.4).
    let fixture = support::fixture("one-line-error");
    let error = expect_create_error(
        GitWorktreeAdapter::new().create_workspace(&WorkspaceRequest {
            repo: fixture.repo.clone(),
            fork: "feat-missing-base".to_owned(),
            base: Some("no-such-revision".to_owned()),
        }),
        "an unknown base fails closed",
    );
    assert!(
        matches!(error, VcsError::CommandFailed { ref command, .. } if command == "git rev-parse"),
        "the failing subcommand is named, got: {error}"
    );
    let message = error.to_string();
    assert!(
        !message.contains('\n'),
        "a VCS failure is exactly one line, got:\n{message}"
    );
    assert!(
        message.starts_with("git rev-parse failed (exit ") && message.contains("fatal:"),
        "the one line carries the exit status and the last stderr line, got: {message}"
    );
}

#[test]
fn sanitised_branch_forks_once_and_a_second_identical_fork_fails_closed() {
    let fixture = support::dirty_fixture("sanitised-branch");
    let fork = sanitize_fork_name("feat/ux-pass").expect("a branch always sanitises to one label");
    assert_eq!(fork, "feat-ux-pass");

    let adapter = GitWorktreeAdapter::new();
    let workspace = adapter
        .create_workspace(&request(&fixture.repo, &fork))
        .expect("the sanitised branch names a fork");
    assert_eq!(workspace.fork_name(), "feat-ux-pass");
    assert!(fixture.ref_exists("refs/heads/feat-ux-pass"));

    let error = expect_create_error(
        adapter.create_workspace(&request(&fixture.repo, &fork)),
        "the same branch never forks twice",
    );
    assert!(
        matches!(error, VcsError::WorkspaceExists { ref fork } if fork == "feat-ux-pass"),
        "{error}"
    );
    workspace.forget().expect("forget the sanitised fork");
}

#[test]
fn snapshot_commit_carries_the_pinned_slipfork_identity() {
    let fixture = support::dirty_fixture("identity");
    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-identity"))
        .expect("dirty fork");
    let wip = fixture.rev(&workspace.refs().wip);
    assert_eq!(
        fixture.git_ok(&["log", "-1", "--format=%an <%ae>|%cn <%ce>", &wip]),
        "slipfork <slipfork@localhost>|slipfork <slipfork@localhost>",
        "the snapshot commit is authored and committed by slipfork, never by the developer's git config"
    );
    workspace.forget().expect("forget");
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
fn fork_succeeds_when_gitignore_ignores_the_state_directories() {
    // The recommended setup for a repository that uses Git worktrees: `.worktrees/` and
    // `.slipfork/` are ignored, so `git add` refuses an explicitly named pathspec for them
    // (`fatal: Pathspec '.worktrees' is in .gitignore`) — the live `M3.26` failure.
    let mut fixture = support::fixture("acme-ignored-state");
    fixture.commit_state_gitignore();
    fixture.make_dirty();
    let sibling = fixture.worktree("feat-sibling");
    fs::create_dir_all(sibling.parent().expect("worktrees parent")).expect("create .worktrees");
    fixture.git_ok(&[
        "worktree",
        "add",
        "-q",
        &sibling.to_string_lossy(),
        "-b",
        "feat-sibling",
        &fixture.base,
    ]);

    let workspace = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-ignored"))
        .expect("a repository that ignores .worktrees/ and .slipfork/ must still fork");
    let wip = fixture.rev("refs/slipfork/feat-ignored/wip");
    assert_eq!(
        fixture.git_ok(&["show", &format!("{wip}:tracked.txt")]),
        "v2",
        "the snapshot still carries the developer's dirty tree"
    );
    let snapshot_paths = fixture.git_ok(&["ls-tree", "-r", "--name-only", &wip]);
    assert_eq!(
        snapshot_paths.lines().collect::<Vec<_>>(),
        [".gitignore", "src/lib.txt", "tracked.txt", "untracked.txt"],
        "the snapshot holds the tracked tree plus the untracked file, never .worktrees/ or .slipfork/"
    );

    // A second fork of the same repository must work too, now that a sibling checkout exists.
    let second = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-second"))
        .expect("a second fork of the same repository still forks");
    let second_paths = fixture.git_ok(&["ls-tree", "-r", "--name-only", &second.refs().wip]);
    assert_eq!(
        second_paths.lines().collect::<Vec<_>>(),
        [".gitignore", "src/lib.txt", "tracked.txt", "untracked.txt"],
        "sibling checkouts stay out of the second snapshot as well"
    );

    workspace.forget().expect("forget the first fork");
    second.forget().expect("forget the second fork");
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
    match error {
        VcsError::ToolMissing {
            tool: ref seen,
            ref hint,
        } => {
            assert_eq!(seen, "git");
            assert!(hint.contains("install Git"), "{hint}");
        }
        other => panic!("expected ToolMissing, got {other}"),
    }
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

#[test]
fn jj_absent_fails_closed_with_the_project_override_hint() {
    if std::env::var_os(MISSING_JJ_CHILD_ENV).is_some() {
        return;
    }
    let empty_path = support::disk_tempdir("empty-path-jj");
    let scratch = support::disk_tempdir("jj-absent-runtime");
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            "jj_absent_child_probe_reports_tool_missing",
            "--nocapture",
        ])
        .env("PATH", empty_path.path())
        .env(MISSING_JJ_CHILD_ENV, scratch.path())
        .output()
        .expect("re-execute the test binary without jj on PATH");
    assert!(
        output.status.success(),
        "child probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("skipped:"),
        "a missing jj is a hard fail-closed path, never a skip"
    );
}

#[test]
fn jj_absent_child_probe_reports_tool_missing() {
    let Some(root) = std::env::var_os(MISSING_JJ_CHILD_ENV) else {
        return;
    };
    let root = Path::new(&root);
    let adapter = JjWorkspaceAdapter::new();
    let error = adapter.detect(root).expect_err("jj is absent from PATH");
    match error {
        VcsError::ToolMissing {
            tool: ref seen,
            ref hint,
        } => {
            assert_eq!(seen, "jj");
            assert!(
                hint.contains("project = \"<slug>\"") && hint.contains("slipfork.toml"),
                "the jj diagnostic points at the project override, got: {hint}"
            );
        }
        other => panic!("expected ToolMissing, got {other}"),
    }

    let root = fs::canonicalize(root).expect("canonicalize the scratch root");
    let repo = RepoRoot {
        vcs_root: root.clone(),
        worktree_root: root,
        project: ProjectSlug::parse("jj-absent").expect("valid scratch slug"),
    };
    let runtime = expect_create_error(
        adapter.create_workspace(&WorkspaceRequest {
            repo,
            fork: "feat-jj".to_owned(),
            base: None,
        }),
        "a fork request without jj fails closed",
    );
    assert!(
        matches!(runtime, VcsError::ToolMissing { ref tool, .. } if tool == "jj"),
        "{runtime}"
    );
}

/// Commit id the protocol double advertises for `refs/heads/feat-billing`.
const BASE_OID: &str = "1111111111111111111111111111111111111111";
/// Commit id the protocol double advertises for `refs/slipfork/feat-billing/wip`.
const WIP_OID: &str = "2222222222222222222222222222222222222222";

/// Serve `requests` against `workspace`, returning the exact response bytes.
fn serve(workspace: &dyn VcsWorkspace, requests: &str) -> String {
    let mut output = Vec::new();
    remote_helper::serve(workspace, Cursor::new(requests.as_bytes()), &mut output)
        .expect("the helper serves the whole command stream");
    String::from_utf8(output).expect("helper responses are UTF-8")
}

/// The byte-exact protocol double every `remote_helper_*` test drives.
fn protocol_double() -> RefWorkspace {
    RefWorkspace::new(BASE_OID, WIP_OID)
}

#[test]
fn remote_helper_capabilities_are_byte_exact() {
    assert_eq!(
        serve(&protocol_double(), "capabilities\n"),
        "fetch\npush\noption\n\n"
    );
}

#[test]
fn remote_helper_list_advertises_base_and_wip_refs() {
    let response = serve(&protocol_double(), "capabilities\nlist\n\n");
    assert_eq!(
        response,
        format!(
            "fetch\npush\noption\n\n{BASE_OID} refs/heads/feat-billing\n{WIP_OID} refs/slipfork/feat-billing/wip\n\n"
        ),
        "list answers one '<sha> <refname>' line per fork ref, space separated, blank terminated"
    );
    assert!(
        !response.contains("refs/heads/feat-billing/wip"),
        "Git's D/F refname lock forbids refs/heads/<fork>/wip"
    );
}

#[test]
fn remote_helper_list_for_push_advertises_the_same_refs() {
    assert_eq!(
        serve(&protocol_double(), "list for-push\n"),
        format!("{BASE_OID} refs/heads/feat-billing\n{WIP_OID} refs/slipfork/feat-billing/wip\n\n")
    );
}

#[test]
fn remote_helper_fetch_batch_verifies_the_advertised_ids() {
    let workspace = protocol_double();
    assert_eq!(
        serve(
            &workspace,
            &format!("fetch {BASE_OID} refs/heads/feat-billing\n\n")
        ),
        "\n",
        "a completed fetch batch is one blank line, with nothing left to transfer"
    );
    assert_eq!(
        serve(
            &workspace,
            &format!(
                "fetch {BASE_OID} refs/heads/feat-billing\nfetch {WIP_OID} refs/slipfork/feat-billing/wip\n\n"
            )
        ),
        "\n"
    );

    let mut output = Vec::new();
    let error = remote_helper::serve(
        &workspace,
        Cursor::new(format!("fetch {WIP_OID} refs/heads/feat-billing\n\n").as_bytes()),
        &mut output,
    )
    .expect_err("a sha that does not match the advertised ref fails closed");
    assert!(
        matches!(error, RemoteHelperError::Malformed { .. }),
        "{error}"
    );
    assert!(output.is_empty(), "nothing is written for a refused fetch");

    let mut output = Vec::new();
    let error = remote_helper::serve(
        &workspace,
        Cursor::new(format!("fetch {BASE_OID} refs/heads/unadvertised\n\n").as_bytes()),
        &mut output,
    )
    .expect_err("an unadvertised ref fails closed");
    assert!(
        matches!(error, RemoteHelperError::Malformed { .. }),
        "{error}"
    );

    let mut output = Vec::new();
    let error = remote_helper::serve(
        &workspace,
        Cursor::new(format!("fetch {BASE_OID} refs/heads/feat-billing\n").as_bytes()),
        &mut output,
    )
    .expect_err("an unterminated batch fails closed");
    assert!(
        matches!(error, RemoteHelperError::Malformed { .. }),
        "{error}"
    );
}

#[test]
fn remote_helper_push_is_refused_for_every_ref() {
    let response = serve(
        &protocol_double(),
        "push refs/heads/feat-billing:refs/heads/feat-billing\npush +HEAD:refs/heads/other\n\n",
    );
    assert_eq!(
        response
            .lines()
            .filter(|line| line.starts_with("error "))
            .count(),
        2,
        "every pushed ref is refused individually, got:\n{response}"
    );
    assert!(response.contains("error refs/heads/feat-billing "));
    assert!(response.contains("error refs/heads/other "));
    assert!(
        response.ends_with("\n\n"),
        "the status report is blank terminated, got:\n{response}"
    );
}

#[test]
fn remote_helper_unknown_verb_answers_unsupported_and_keeps_serving() {
    assert_eq!(
        serve(
            &protocol_double(),
            "export\nexport refs/heads/x\ncapabilities\n"
        ),
        "unsupported\nunsupported\nfetch\npush\noption\n\n",
        "an unknown verb is answered, never fatal"
    );
}

#[test]
fn remote_helper_option_responses_follow_the_documented_vocabulary() {
    assert_eq!(
        serve(
            &protocol_double(),
            "option progress false\noption verbosity 2\noption depth 1\noption verbosity 9\n"
        ),
        "ok\nok\nunsupported\nerror unsupported verbosity value '9'\n"
    );
}

#[test]
fn remote_helper_rejects_oversized_frames() {
    let oversized = "x".repeat(MAX_FRAME_BYTES + 8);
    let mut output = Vec::new();
    let error = remote_helper::serve(
        &protocol_double(),
        Cursor::new(oversized.as_bytes()),
        &mut output,
    )
    .expect_err("an oversized frame fails closed");
    assert!(
        matches!(error, RemoteHelperError::FrameTooLarge { .. }),
        "{error}"
    );
}

#[test]
fn remote_helper_git_fetch_round_trip_from_a_scratch_repository() {
    let fixture = support::dirty_fixture("acme-app");
    let workspace: Box<dyn VcsWorkspace> = GitWorktreeAdapter::new()
        .create_workspace(&request(&fixture.repo, "feat-billing"))
        .expect("dirty fork workspace");
    let base = fixture.rev("refs/heads/feat-billing");
    let wip = fixture.rev("refs/slipfork/feat-billing/wip");
    assert_ne!(base, wip, "the dirty fork advertises two distinct commits");

    // A real `git fetch` speaks to the real handler: `git-remote-slipfork` is a relay script that
    // moves bytes between Git and two FIFOs, while the protocol itself runs in this process.
    let scratch = support::disk_tempdir("remote-helper-round-trip");
    let bin = scratch.path().join("bin");
    fs::create_dir_all(&bin).expect("create the helper directory");
    let requests = scratch.path().join("requests");
    let responses = scratch.path().join("responses");
    mkfifo(&requests);
    mkfifo(&responses);
    let helper = bin.join("git-remote-slipfork");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nexec 3<&0\ncat <&3 > '{}' &\nexec cat '{}'\n",
            requests.display(),
            responses.display()
        ),
    )
    .expect("write the relay helper");
    let mut permissions = fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&helper, permissions).expect("make the helper executable");

    let server = std::thread::spawn(move || {
        let requests = fs::File::open(&requests).expect("open the request fifo");
        let responses = fs::File::options()
            .write(true)
            .open(&responses)
            .expect("open the response fifo");
        remote_helper::serve(
            workspace.as_ref(),
            std::io::BufReader::new(requests),
            responses,
        )
        .expect("serve the fork workspace to git");
    });

    support::git_ok_in(
        fixture.root(),
        &["remote", "add", "slipfork", "slipfork://test"],
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").expect("PATH is set for the test process")
    );
    let root = fixture.root().display().to_string();
    let output = Command::new("timeout")
        .args([
            "--signal=KILL",
            "120",
            "git",
            "-C",
            &root,
            "fetch",
            "slipfork",
        ])
        .args([
            "refs/heads/feat-billing:refs/remotes/slipfork/feat-billing",
            "+refs/slipfork/feat-billing/wip:refs/slipfork/fetched/wip",
        ])
        .env("PATH", &path)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run a real git fetch through the remote helper");
    assert!(
        output.status.success(),
        "git fetch slipfork failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().expect("the helper session finishes");

    assert_eq!(
        fixture.rev("refs/remotes/slipfork/feat-billing"),
        base,
        "`git fetch slipfork <fork>` moved refs/heads/<fork> into the fetching repository"
    );
    assert_eq!(
        fixture.rev("refs/slipfork/fetched/wip"),
        wip,
        "`git fetch slipfork '+refs/slipfork/<fork>/wip:...'` moved the snapshot ref"
    );
}
