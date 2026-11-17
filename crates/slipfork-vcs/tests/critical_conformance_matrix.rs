//! Protected conformance matrix for every `VcsWorkspaceFactory` (`M2.11` `NODE.md` #5, reused
//! verbatim by `M2.12`).
//!
//! This file is deliberately named `critical_*` so `tools/critical_guard.py` protects the matrix
//! body itself: weakening an assertion here cannot slip through as a change to an unprotected test
//! helper. It is compiled **only** as `matrix` inside `tests/critical_vcs.rs`
//! (`autotests = false` in `Cargo.toml`), and it borrows fixtures and builders from `tests/support`.

use std::path::PathBuf;
use std::sync::Arc;

use slipfork_vcs::{
    GitRepoRootResolver, NoProjectOverride, RepoRootResolver, VcsError, VcsWorkspaceFactory,
    WorkspaceChanges, WorkspaceRefs,
};

use crate::support::{ConformanceRepos, FixedOverride, request};

/// The shared `VcsWorkspace` conformance matrix (`M2.11` `NODE.md` #5, reused verbatim by `M2.12`).
///
/// Covered for every adapter and every repository flavour: clean fork with `wip == base`, dirty fork
/// with untracked + modified + deleted files whose two refs coexist as distinct commits, base
/// ancestry, `.worktrees/<fork>` inside the repository, a second sibling fork staying clean, source
/// `ProjectSlugOverride` precedence, repeated-create refusal, `forget()` idempotency, and the
/// D/F-safe `refs/heads/<fork>` + `refs/slipfork/<fork>/wip` layout resolved through the port.
pub fn assert_vcs_workspace_conformance(
    factory: &dyn VcsWorkspaceFactory,
    repos: &dyn ConformanceRepos,
) {
    let fixture = repos.dirty("acme-app");

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
