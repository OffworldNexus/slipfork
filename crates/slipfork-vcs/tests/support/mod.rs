//! Dev-only support for `slipfork-vcs` integration tests: hermetic, disk-backed Git fixtures plus
//! the shared [`assert_vcs_workspace_conformance`] matrix every `VcsWorkspaceFactory` must pass.
//!
//! Compiled only into `tests/` targets. Every repository lives under
//! `$HOME/.cache/slipfork-critical-tests` (never `/tmp` `tmpfs`), and every Git and Jujutsu
//! invocation scrubs the ambient `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR`
//! variables so the test process environment can never redirect a probe.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use slipfork_vcs::{
    ProjectSlugOverride, RepoRoot, VcsError, VcsWorkspace, WorkspaceChanges, WorkspaceRefs,
    WorkspaceRequest,
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

    /// Turn the clean working tree into the conformance matrix's dirty example.
    ///
    /// One modified, one deleted, and one untracked path is enough to exercise every bucket the
    /// `WorkspaceChanges` port reports.
    pub fn make_dirty(&self) {
        self.write("tracked.txt", "v2\n");
        self.remove("deleted.txt");
        self.write("untracked.txt", "new\n");
    }

    /// Commit `.gitignore` entries for Slipfork's in-repo state directories.
    ///
    /// A fork checkout lives at `.worktrees/<fork>`, and Jujutsu snapshots every non-ignored path
    /// into the working-copy commit, so a repository with in-repo forks must ignore them (exactly
    /// like a Git repository that does not want its own fork checkouts in `git status`).
    pub fn commit_state_gitignore(&mut self) {
        self.write(".gitignore", ".worktrees/\n.slipfork/\n");
        self.git_ok(&["add", "--", ".gitignore"]);
        self.git_ok(&["commit", "-q", "-m", "ignore slipfork state"]);
        self.base = self.git_ok(&["rev-parse", "HEAD"]);
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
    fixture.make_dirty();
    fixture
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
}

/// Build a colocated Jujutsu repository from a Git fixture.
fn jj_fixture(name: &str) -> Fixture {
    let mut fixture = fixture(name);
    fixture.commit_state_gitignore();
    jj_ok_in(fixture.root(), &["git", "init", "--colocate"]);
    fixture
}

/// One raw Jujutsu invocation inside `cwd` with a hermetic environment.
pub fn jj_in(cwd: &Path, args: &[&str]) -> Output {
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
pub fn jj_ok_in(cwd: &Path, args: &[&str]) -> String {
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
pub fn tool_missing_skip_line(error: &VcsError) -> Option<String> {
    match error {
        VcsError::ToolMissing { tool, .. } => Some(format!("skipped: ToolMissing({tool})")),
        _ => None,
    }
}

/// Create a named FIFO, so a test can relay Git's helper protocol into an in-process handler.
pub fn mkfifo(path: &Path) {
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
pub struct RefWorkspace {
    pub base: String,
    pub wip: String,
}

impl RefWorkspace {
    pub fn new(base: &str, wip: &str) -> Self {
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
