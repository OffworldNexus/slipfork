//! Protected conformance matrix for every `VcsWorkspaceFactory` (`M2.11` `NODE.md` #5, reused
//! verbatim by `M2.12`; extended by `M3.28.5` with the branch probe).
//!
//! This file is deliberately named `critical_*` so `tools/critical_guard.py` protects the matrix
//! body itself: weakening an assertion here cannot slip through as a change to an unprotected test
//! helper. It is its own test target (`autotests = false` in `Cargo.toml`) and borrows fixtures and
//! builders from `tests/support`.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use slipfork_vcs::{
    GitRepoRootResolver, GitWorktreeAdapter, JjWorkspaceAdapter, NoProjectOverride,
    RepoRootResolver, VcsError, VcsWorkspaceFactory, WorkspaceChanges, WorkspaceRefs,
};

use crate::support::{
    FixedOverride, Fixture, dirty_fixture, expect_create_error, fixture, request,
};

/// The Git family's half of the matrix (`M3.28.5`); Jujutsu is covered identically below.
#[test]
fn git_conformance_matrix_satisfies_shared_suite() {
    assert_vcs_workspace_conformance(&GitWorktreeAdapter::new(), &GitRepos);
}

/// The Jujutsu family's half of the matrix; a host without `jj` skips **loudly**, never silently.
#[test]
fn jj_conformance_matrix_satisfies_shared_suite() {
    let adapter = JjWorkspaceAdapter::new();
    let probe_root = support::disk_tempdir("jj-detect");
    match adapter.detect(probe_root.path()) {
        // The identical matrix the Git adapter passes, run against a colocated Jujutsu repository.
        Ok(()) => assert_vcs_workspace_conformance(&adapter, &JjRepos),
        Err(error) => {
            // A host without Jujutsu skips exactly once, and says so on stderr: a silent `ok` would
            // let the whole Jujutsu half of the matrix disappear unnoticed.
            let line = tool_missing_skip_line(&error)
                .unwrap_or_else(|| panic!("the jj probe failed unexpectedly: {error}"));
            assert_eq!(line, "skipped: ToolMissing(jj)");
            eprintln!(
                "{line}: install Jujutsu (https://jj-vcs.github.io) to run the Jujutsu half of the matrix"
            );

            // ... and the runtime path still fails closed, with no Git fallback and no artifacts.
            let fixture = support::fixture("jj-absent-conformance");
            let runtime = expect_create_error(
                adapter.create_workspace(&request(&fixture.repo, "feat-absent")),
                "create_workspace without jj fails closed",
            );
            assert!(
                matches!(runtime, VcsError::ToolMissing { ref tool, .. } if tool == "jj"),
                "{runtime}"
            );
            assert!(
                !fixture.worktree("feat-absent").exists()
                    && !fixture.ref_exists("refs/heads/feat-absent"),
                "a refused Jujutsu fork leaves no artifacts behind"
            );
        }
    }
}

/// The shared `VcsWorkspace` conformance matrix (`M2.11` `NODE.md` #5, reused verbatim by `M2.12`).
///
/// Covered for every adapter and every repository flavour: the branch probe answering `Some` for a
/// branch/bookmarked checkout and `None` for a detached/bookmark-less one, clean fork with
/// `wip == base`, dirty fork with untracked + modified + deleted files whose two refs coexist as
/// distinct commits, base ancestry, `.worktrees/<fork>` inside the repository, a second sibling fork
/// staying clean, source `ProjectSlugOverride` precedence, repeated-create refusal, `forget()`
/// idempotency, and the D/F-safe `refs/heads/<fork>` + `refs/slipfork/<fork>/wip` layout resolved
/// through the port.
pub fn assert_vcs_workspace_conformance(
    factory: &dyn VcsWorkspaceFactory,
    repos: &dyn ConformanceRepos,
) {
    let fixture = repos.dirty("acme-app");

    assert_eq!(
        factory
            .current_branch(&fixture.repo)
            .expect("the branch probe answers for a resolved repository"),
        repos.expected_branch().map(str::to_owned),
        "current_branch reports a branch checkout's branch and None for a detached / bookmark-less one"
    );

    // The other half of the probe: a checkout that *does* carry a branch must answer `Some`, so an
    // implementation that always returned `None` cannot pass by only ever seeing the bare case.
    let bookmarked = repos.bookmarked("acme-bookmarked");
    assert_eq!(
        factory
            .current_branch(&bookmarked.repo)
            .expect("the branch probe answers for a bookmarked checkout"),
        Some("main".to_owned()),
        "a checkout whose working copy carries the `main` branch/bookmark answers Some(\"main\")"
    );

    let dirty_request = request(&fixture.repo, "feat-dirty");
    let dirty = factory
        .create_workspace(&dirty_request)
        .expect("dirty fork is created");
    assert_eq!(dirty.fork_name(), "feat-dirty");
    assert_eq!(dirty.path(), fixture.worktree("feat-dirty"));
    assert!(
        dirty.path().starts_with(fixture.root()),
        ".worktrees/<fork> lives inside the repository, got {}",
        dirty.path().display()
    );
    assert!(
        !dirty.path().starts_with("/tmp"),
        ".worktrees/<fork> must never live on /tmp tmpfs"
    );
    assert_eq!(
        dirty.refs(),
        WorkspaceRefs {
            base: "refs/heads/feat-dirty".to_owned(),
            wip: "refs/slipfork/feat-dirty/wip".to_owned(),
        }
    );
    assert_eq!(
        fixture.rev("refs/heads/feat-dirty"),
        fixture.base,
        "the fork branch starts at the requested base commit"
    );
    assert_eq!(
        dirty
            .resolve(&dirty.refs().base)
            .expect("base ref resolves"),
        fixture.base,
        "the port resolves refs/heads/<fork> to the advertised commit"
    );
    let wip = fixture.rev(&dirty.refs().wip);
    assert_eq!(
        dirty.resolve(&dirty.refs().wip).expect("wip ref resolves"),
        wip,
        "the port resolves refs/slipfork/<fork>/wip to the advertised commit"
    );
    assert_ne!(
        wip, fixture.base,
        "a dirty tree must produce a snapshot commit"
    );
    assert_eq!(
        fixture.rev(&format!("{wip}^")),
        fixture.base,
        "the snapshot commit's parent is the fork base"
    );
    assert!(fixture.is_ancestor(&fixture.base, &wip));
    assert_eq!(
        fixture.git_ok(&["show", &format!("{wip}:tracked.txt")]),
        "v2"
    );
    assert_eq!(
        fixture.git_ok(&["show", &format!("{wip}:untracked.txt")]),
        "new"
    );
    assert!(!fixture.object_exists(&format!("{wip}:deleted.txt")));
    let snapshot_paths = fixture.git_ok(&["ls-tree", "-r", "--name-only", &wip]);
    assert!(
        !snapshot_paths
            .lines()
            .any(|path| path == ".slipfork" || path.starts_with(".slipfork/")),
        "Slipfork scratch state may never enter a fork snapshot, got:\n{snapshot_paths}"
    );

    let changes = dirty.changes().expect("dirty changes are readable");
    assert!(!changes.truncated);
    assert_eq!(
        changes.untracked,
        vec![PathBuf::from("untracked.txt")],
        "untracked files are reported separately"
    );
    assert_eq!(
        changes.tracked.len(),
        2,
        "one modification and one deletion"
    );
    assert!(changes.tracked.contains(&PathBuf::from("tracked.txt")));
    assert!(changes.tracked.contains(&PathBuf::from("deleted.txt")));

    assert!(
        matches!(
            factory.create_workspace(&dirty_request),
            Err(VcsError::WorkspaceExists { fork }) if fork == "feat-dirty"
        ),
        "a second create_workspace for the same fork is refused, never overwritten"
    );

    dirty.forget().expect("forget removes the dirty fork");
    dirty.forget().expect("forget is idempotent");
    assert!(!dirty.path().exists());
    assert!(!fixture.ref_exists("refs/heads/feat-dirty"));
    assert!(!fixture.ref_exists("refs/slipfork/feat-dirty/wip"));
    assert!(!fixture.scratch_index("feat-dirty").exists());

    let fixture = repos.clean("acme-app");

    let clean = factory
        .create_workspace(&request(&fixture.repo, "feat-clean"))
        .expect("clean fork is created");
    assert_eq!(fixture.rev("refs/heads/feat-clean"), fixture.base);
    assert_eq!(
        fixture.rev(&clean.refs().wip),
        fixture.base,
        "a clean tree points refs/slipfork/<fork>/wip directly at the base commit"
    );
    assert_eq!(
        clean.resolve(&clean.refs().wip).expect("wip ref resolves"),
        fixture.base,
        "a clean fork's two refs resolve to the same commit"
    );
    assert_eq!(
        clean.changes().expect("clean changes are readable"),
        WorkspaceChanges::default()
    );
    assert!(
        matches!(
            factory.create_workspace(&request(&fixture.repo, "feat-clean")),
            Err(VcsError::WorkspaceExists { fork }) if fork == "feat-clean"
        ),
        "repeated clean create is refused too"
    );
    clean.forget().expect("forget removes the clean fork");
    clean.forget().expect("clean forget is idempotent");
    assert!(!fixture.ref_exists("refs/heads/feat-clean"));
    assert!(!fixture.ref_exists("refs/slipfork/feat-clean/wip"));

    // A second fork of an otherwise clean tree must not embed the first fork's checkout: Git would
    // stage `.worktrees/<first>` as a `160000 commit …/.worktrees/<first>` gitlink.
    let first = factory
        .create_workspace(&request(&fixture.repo, "feat-first"))
        .expect("first sibling fork is created");
    let second = factory
        .create_workspace(&request(&fixture.repo, "feat-second"))
        .expect("second sibling fork is created");
    assert_eq!(
        fixture.rev(&second.refs().wip),
        fixture.base,
        "a clean tree stays clean when a sibling checkout already exists"
    );
    let snapshot_paths = fixture.git_ok(&["ls-tree", "-r", "--name-only", &second.refs().wip]);
    assert!(
        !snapshot_paths
            .lines()
            .any(|path| path == ".worktrees" || path.starts_with(".worktrees/")),
        "no sibling checkout may be embedded in the snapshot, got:\n{snapshot_paths}"
    );
    let snapshot_tree = fixture.git_ok(&["ls-tree", "-r", &second.refs().wip]);
    assert!(
        !snapshot_tree.lines().any(|line| line.starts_with("160000")),
        "a sibling checkout must never be staged as an embedded-repository gitlink, got:\n{snapshot_tree}"
    );
    assert_eq!(
        second.changes().expect("sibling changes are readable"),
        WorkspaceChanges::default()
    );
    first.forget().expect("forget removes the first sibling");
    second.forget().expect("forget removes the second sibling");

    let plain = GitRepoRootResolver::new(Arc::new(NoProjectOverride))
        .resolve(fixture.root())
        .expect("fixture root resolves");
    assert_eq!(plain.vcs_root, fixture.repo.vcs_root);
    assert_eq!(plain.worktree_root, fixture.repo.vcs_root);
    assert_eq!(plain.project, fixture.repo.project);

    let (override_impl, seen) = FixedOverride::new("fixture-override");
    let overridden = GitRepoRootResolver::new(Arc::new(override_impl))
        .resolve(&fixture.path("src"))
        .expect("override resolves");
    assert_eq!(
        overridden.project.as_str(),
        "fixture-override",
        "ProjectSlugOverride beats the directory basename"
    );
    assert_eq!(
        seen.lock().expect("override lock").as_deref(),
        Some(fixture.repo.vcs_root.as_path()),
        "the override is asked about <vcs_root>, never a subdirectory"
    );
}

/// Fixture assertions only the matrix needs: object identity and commit ancestry.
impl Fixture {
    /// `true` when `ancestor` is an ancestor of `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.git(&["merge-base", "--is-ancestor", ancestor, descendant])
            .status
            .success()
    }

    /// `true` when `rev:path` resolves to a blob/tree inside the repository.
    pub fn object_exists(&self, object: &str) -> bool {
        self.git(&["cat-file", "-e", object]).status.success()
    }
}

/// Builds the repository flavours the shared conformance matrix runs against.
///
/// The matrix is identical for every adapter; only the repository a fork is created from differs
/// (a plain Git checkout, or a Jujutsu checkout with a colocated Git store).
pub trait ConformanceRepos {
    /// A repository whose working tree has one modified, one deleted, and one untracked file.
    fn dirty(&self, name: &str) -> Fixture;
    /// A repository with a clean working tree.
    fn clean(&self, name: &str) -> Fixture;
    /// The branch [`VcsWorkspaceFactory::current_branch`] must report for a fresh fixture.
    ///
    /// A Git fixture is checked out on `main`; a colocated Jujutsu fixture's working-copy commit sits
    /// on top of the imported `main` commit and therefore carries no local bookmark of its own.
    fn expected_branch(&self) -> Option<&'static str>;
    /// A repository whose requesting checkout carries the `main` branch/bookmark.
    fn bookmarked(&self, name: &str) -> Fixture;
}

/// Git repository flavour: the matrix runs against an ordinary checkout.
pub struct GitRepos;

impl ConformanceRepos for GitRepos {
    fn dirty(&self, name: &str) -> Fixture {
        dirty_fixture(name)
    }

    fn clean(&self, name: &str) -> Fixture {
        fixture(name)
    }

    fn expected_branch(&self) -> Option<&'static str> {
        Some("main")
    }

    fn bookmarked(&self, name: &str) -> Fixture {
        // A Git fixture is created on `main`, so its checkout already carries the branch.
        fixture(name)
    }
}

/// Jujutsu repository flavour: a Git fixture with Slipfork's state directories ignored, then
/// `jj git init --colocate` so fork refs are Git-fetchable.
pub struct JjRepos;

impl ConformanceRepos for JjRepos {
    fn dirty(&self, name: &str) -> Fixture {
        let fixture = jj_fixture(name);
        fixture.make_dirty();
        fixture
    }

    fn clean(&self, name: &str) -> Fixture {
        jj_fixture(name)
    }

    fn expected_branch(&self) -> Option<&'static str> {
        None
    }

    fn bookmarked(&self, name: &str) -> Fixture {
        let fixture = jj_fixture(name);
        // `set` rather than `create`: colocation imports Git's `main` branch as a bookmark on the
        // parent commit, so `create main` would refuse; `set` puts `main` on the working copy, which
        // is exactly the "booked working copy" the probe must report.
        jj_ok_in(fixture.root(), &["bookmark", "set", "main", "-r", "@"]);
        fixture
    }
}

/// Build a colocated Jujutsu repository from a Git fixture.
fn jj_fixture(name: &str) -> Fixture {
    let mut fixture = fixture(name);
    fixture.commit_state_gitignore();
    jj_ok_in(fixture.root(), &["git", "init", "--colocate"]);
    fixture
}

/// One raw Jujutsu invocation inside `cwd` with a hermetic environment.
fn jj_in(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("jj");
    command
        .current_dir(cwd)
        .args(args)
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env("JJ_USER", "Slipfork Test")
        .env("JJ_EMAIL", "test@localhost");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    command.output().expect("spawn jj")
}

/// [`jj_in`] asserting a zero exit, returning trimmed stdout.
fn jj_ok_in(cwd: &Path, args: &[&str]) -> String {
    let output = jj_in(cwd, args);
    assert!(
        output.status.success(),
        "jj {:?} failed in {}: {}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("jj stdout is UTF-8")
        .trim()
        .to_owned()
}

/// The one explicit line a hermetic conformance runner prints when an adapter's tool is absent.
///
/// A family whose tool is missing must skip **loudly** (`skipped: ToolMissing(jj)`), never pass
/// silently: the caller still fails the run when the Git family itself cannot execute.
fn tool_missing_skip_line(error: &VcsError) -> Option<String> {
    match error {
        VcsError::ToolMissing { tool, .. } => Some(format!("skipped: ToolMissing({tool})")),
        _ => None,
    }
}
