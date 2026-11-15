//! Dev-only support for `slipfork-vcs` integration tests: hermetic, disk-backed Git fixtures plus
//! the shared [`assert_vcs_workspace_conformance`] matrix every `VcsWorkspaceFactory` must pass.
//!
//! Compiled only into `tests/` targets. Every repository lives under
//! `$HOME/.cache/slipfork-critical-tests` (never `/tmp` `tmpfs`), and every Git invocation scrubs
//! the ambient `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR` variables so the
//! test process environment can never redirect a probe.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use slipfork_vcs::{
    GitRepoRootResolver, NoProjectOverride, ProjectSlugOverride, RepoRoot, RepoRootResolver,
    VcsError, VcsWorkspaceFactory, WorkspaceChanges, WorkspaceRefs, WorkspaceRequest,
};
use slipfork_world::ProjectSlug;
use tempfile::TempDir;

/// One hermetic Git repository plus the base commit every fork starts from.
pub struct Fixture {
    _temp: TempDir,
    pub repo: RepoRoot,
    pub base: String,
}

impl Fixture {
    /// Repository root (`<vcs_root>`), canonicalized.
    pub fn root(&self) -> &Path {
        &self.repo.vcs_root
    }

    /// Absolute path of `relative` inside the repository.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    /// Write `contents` to `relative`, creating parent directories.
    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent directory");
        }
        fs::write(&path, contents).expect("write fixture file");
    }

    /// Remove `relative` from the working tree.
    pub fn remove(&self, relative: &str) {
        fs::remove_file(self.path(relative)).expect("remove fixture file");
    }

    /// Run Git inside the repository root.
    pub fn git(&self, args: &[&str]) -> Output {
        git_in(self.root(), args)
    }

    /// Run Git inside the repository root, asserting success, returning trimmed stdout.
    pub fn git_ok(&self, args: &[&str]) -> String {
        git_ok_in(self.root(), args)
    }

    /// Resolve `rev` to an object id.
    pub fn rev(&self, rev: &str) -> String {
        self.git_ok(&["rev-parse", "--verify", rev])
    }

    /// `true` when `reference` exists in the repository's ref store.
    pub fn ref_exists(&self, reference: &str) -> bool {
        self.git(&["show-ref", "--verify", "--quiet", reference])
            .status
            .success()
    }

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

    /// Path of the fork checkout (`.worktrees/<fork>`).
    pub fn worktree(&self, fork: &str) -> PathBuf {
        self.root().join(".worktrees").join(fork)
    }

    /// Path of the scratch index the adapter uses to synthesize `fork`'s dirty view.
    pub fn scratch_index(&self, fork: &str) -> PathBuf {
        slipfork_vcs::git::scratch_index_path(self.root(), fork)
    }

    /// `(inode, mtime seconds, mtime nanoseconds)` of the live `.git/index`.
    ///
    /// The fork synthesizer must never open, rename, or lock this file, so these values are the
    /// protected invariant for a dirty fork of a large working tree.
    pub fn index_stat(&self) -> (u64, i64, i64) {
        let index = self.root().join(".git").join("index");
        let metadata = fs::metadata(&index).expect("live .git/index exists");
        (metadata.ino(), metadata.mtime(), metadata.mtime_nsec())
    }
}

/// Build a clean fixture repository with one modified, one deleted, and one untracked candidate.
pub fn fixture(name: &str) -> Fixture {
    let temp = disk_tempdir(name);
    let root = temp.path().join(name);
    fs::create_dir_all(&root).expect("create fixture repository directory");
    let root = fs::canonicalize(&root).expect("canonicalize fixture repository");
    git_in(&root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("tracked.txt"), "v1\n").expect("write tracked.txt");
    fs::write(root.join("deleted.txt"), "gone\n").expect("write deleted.txt");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("src").join("lib.txt"), "lib\n").expect("write src/lib.txt");
    git_in(
        &root,
        &["add", "--", "tracked.txt", "deleted.txt", "src/lib.txt"],
    );
    git_in(&root, &["commit", "-q", "-m", "init"]);
    let base = git_ok_in(&root, &["rev-parse", "HEAD"]);
    let project = ProjectSlug::parse(name).expect("fixture names are valid project slugs");
    Fixture {
        _temp: temp,
        repo: RepoRoot {
            vcs_root: root.clone(),
            worktree_root: root,
            project,
        },
        base,
    }
}

/// Build a fixture whose working tree has one modified, one deleted, and one untracked file.
pub fn dirty_fixture(name: &str) -> Fixture {
    let fixture = fixture(name);
    fixture.write("tracked.txt", "v2\n");
    fixture.remove("deleted.txt");
    fixture.write("untracked.txt", "new\n");
    fixture
}

/// One `create_workspace` request for `repo`; `None` forks the requesting checkout's `HEAD`.
pub fn request(repo: &RepoRoot, fork: &str) -> WorkspaceRequest {
    WorkspaceRequest {
        repo: repo.clone(),
        fork: fork.to_owned(),
        base: None,
    }
}

/// [`ProjectSlugOverride`] double that fixes the slug and records which root it was asked about.
pub struct FixedOverride {
    slug: ProjectSlug,
    seen: Arc<Mutex<Option<PathBuf>>>,
}

impl FixedOverride {
    pub fn new(slug: &str) -> (Self, Arc<Mutex<Option<PathBuf>>>) {
        let seen = Arc::new(Mutex::new(None));
        (
            Self {
                slug: ProjectSlug::parse(slug).expect("valid override slug"),
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

impl ProjectSlugOverride for FixedOverride {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        *self.seen.lock().expect("override lock") = Some(vcs_root.to_path_buf());
        Ok(Some(self.slug.clone()))
    }
}

/// [`ProjectSlugOverride`] double proving adapter errors are propagated verbatim.
pub struct FailingOverride;

impl ProjectSlugOverride for FailingOverride {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        Err(VcsError::Io {
            path: vcs_root.to_path_buf(),
            message: "slipfork.toml is unreadable".to_owned(),
        })
    }
}

/// The shared `VcsWorkspace` conformance matrix (`M2.11` `NODE.md` #5, reused by `M2.12`).
///
/// Covered for every adapter: clean fork with `wip == base`, dirty fork with untracked + modified +
/// deleted files whose two refs coexist as distinct commits, base ancestry, `.worktrees/<fork>`
/// inside the repository, a second sibling fork staying clean, source `ProjectSlugOverride`
/// precedence, repeated-create refusal, `forget()` idempotency, and an untouched live `.git/index`.
pub fn assert_vcs_workspace_conformance(factory: &dyn VcsWorkspaceFactory) {
    let fixture = dirty_fixture("acme-app");
    let index_before = fixture.index_stat();

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
    let wip = fixture.rev(&dirty.refs().wip);
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

    let status = git_stdout_in(dirty.path(), &["status", "--porcelain=v1"]);
    for expected in [" M tracked.txt", " D deleted.txt", "?? untracked.txt"] {
        assert!(
            status.lines().any(|line| line == expected),
            "expected '{expected}' in the fork's developer view, got:\n{status}"
        );
    }

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

    assert_eq!(
        fixture.index_stat(),
        index_before,
        "the live .git/index inode and mtime must be untouched by a dirty fork"
    );
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

    fixture.git_ok(&["checkout", "--", "."]);
    fixture.remove("untracked.txt");

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
    assert_eq!(plain.project.as_str(), "acme-app");

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

/// Allocate a disk-backed temporary directory under `$HOME/.cache/slipfork-critical-tests`.
pub fn disk_tempdir(name: &str) -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache")
        .join("slipfork-critical-tests");
    fs::create_dir_all(&base).expect("create disk test cache base");
    tempfile::Builder::new()
        .prefix(&format!("vcs-{name}-"))
        .tempdir_in(base)
        .expect("create disk tempdir")
}

/// One raw Git invocation inside `cwd` with a hermetic, scrubbed environment.
pub fn git_in(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "user.name=Slipfork Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(OsStr::new(key));
    }
    command.output().expect("spawn git")
}

/// [`git_in`] asserting a zero exit, returning stdout verbatim (leading/trailing whitespace kept).
pub fn git_stdout_in(cwd: &Path, args: &[&str]) -> String {
    let output = git_in(cwd, args);
    assert!(
        output.status.success(),
        "git {:?} failed in {}: {}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout is UTF-8")
}

/// [`git_in`] asserting a zero exit, returning trimmed stdout.
pub fn git_ok_in(cwd: &Path, args: &[&str]) -> String {
    git_stdout_in(cwd, args).trim().to_owned()
}
