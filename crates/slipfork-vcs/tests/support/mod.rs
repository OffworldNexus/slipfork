//! Dev-only support shared by both `slipfork-vcs` integration-test targets: hermetic, disk-backed
//! Git fixtures plus the request/override/runtime helpers each critical suite uses.
//!
//! Compiled only into `tests/` targets. Every repository lives under
//! `$HOME/.cache/slipfork-critical-tests` (never `/tmp` `tmpfs`), and every Git invocation scrubs the
//! ambient `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR` variables so the test
//! process environment can never redirect a probe.
//!
//! Helpers that only one target needs live there, not here: the conformance matrix's repository
//! flavours are declared in `tests/critical_conformance_matrix.rs`, and the remote-helper doubles plus
//! `index_stat` in `tests/critical_vcs.rs`. That keeps each target's build free of dead helpers
//! without weakening a lint.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use slipfork_vcs::{ProjectSlugOverride, RepoRoot, VcsError, VcsWorkspace, WorkspaceRequest};
use slipfork_world::ProjectSlug;
use tempfile::TempDir;

/// Extract a failed `create_workspace` error (`Box<dyn VcsWorkspace>` is not `Debug`).
pub fn expect_create_error(
    result: Result<Box<dyn VcsWorkspace>, VcsError>,
    context: &str,
) -> VcsError {
    match result {
        Ok(_) => panic!("{context}: expected an error, got a workspace"),
        Err(error) => error,
    }
}

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

    /// Path of the fork checkout (`.worktrees/<fork>`).
    pub fn worktree(&self, fork: &str) -> PathBuf {
        self.root().join(".worktrees").join(fork)
    }

    /// Path of the scratch index the adapter uses to synthesize `fork`'s dirty view.
    pub fn scratch_index(&self, fork: &str) -> PathBuf {
        slipfork_vcs::git::scratch_index_path(self.root(), fork)
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
