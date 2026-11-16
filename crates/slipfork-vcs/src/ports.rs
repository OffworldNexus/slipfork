//! Domain ports: `ProjectSlugOverride`, `RepoRootResolver`, `VcsWorkspaceFactory`, `VcsWorkspace`,
//! and their `RepoRoot` / `WorkspaceRequest` / `WorkspaceRefs` / `WorkspaceChanges` value types.
//!
//! Zero subprocess spawning, zero environment reads, zero clock reads: adapters own all ambient
//! access, and no backend-kind discriminator exists outside `slipforkd`'s factory.

use std::path::{Path, PathBuf};

use slipfork_world::{ProjectSlug, WorldError};
use thiserror::Error;

/// Repository-relative directory holding every fork checkout.
///
/// Shared by both adapters: a fork workspace always lives at `<vcs_root>/.worktrees/<fork>`, inside
/// the repository and never on a tmpfs.
pub const WORKTREES_DIRECTORY: &str = ".worktrees";

/// Hard cap on the paths a single [`VcsWorkspace::changes`] call reports.
///
/// The dirty view of a large polyglot checkout can hold tens of thousands of untracked files; every
/// consumer (`git-remote-slipfork`, `slipfork list`, hook diagnostics) only needs a bounded preview,
/// so the adapters stop collecting at this many paths and set [`WorkspaceChanges::truncated`].
pub const MAX_WORKSPACE_CHANGES: usize = 500;

/// Errors produced by VCS ports and their Git / Jujutsu adapters.
///
/// Every variant is fail-closed and actionable: there is no "not a repository, falling back to the
/// current directory" arm, and no adapter swallows a Git diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VcsError {
    #[error(
        "not a git or jj repository: {start} (run slipfork from inside a version-controlled checkout)"
    )]
    NotARepository { start: PathBuf },
    #[error("unsupported repository topology '{topology}' at {root}: {hint}")]
    UnsupportedTopology {
        topology: &'static str,
        root: PathBuf,
        hint: String,
    },
    #[error(
        "cannot derive a project slug from repository root {vcs_root}: '{slug}' is not a single lowercase DNS label; set `project = \"<slug>\"` in {hint}"
    )]
    InvalidProjectSlug {
        vcs_root: PathBuf,
        slug: String,
        hint: String,
    },
    #[error(
        "cannot derive a project slug from repository root {vcs_root}: the directory name is not valid UTF-8; set `project = \"<slug>\"` in {hint}"
    )]
    UnnamedProjectRoot { vcs_root: PathBuf, hint: String },
    #[error(
        "fork '{fork}' already exists (refs/heads/{fork}, refs/slipfork/{fork}/wip, or .worktrees/{fork}); refusing to overwrite it"
    )]
    WorkspaceExists { fork: String },
    #[error("required tool '{tool}' was not found on PATH: {hint}")]
    ToolMissing { tool: String, hint: String },
    #[error("failed to spawn `{command}`: {message}")]
    CommandSpawn { command: String, message: String },
    #[error("`{command}` failed ({status}): {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("`{command}` produced unexpected output: {message}")]
    UnexpectedOutput { command: String, message: String },
    #[error("filesystem error at {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error(transparent)]
    World(#[from] WorldError),
}

impl VcsError {
    /// Build [`VcsError::Io`] for a filesystem operation on `path`.
    pub(crate) fn io(path: &Path, err: &std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        }
    }
}

/// One resolved repository: the VCS root that owns `.git`/`.jj`, the checkout the request came
/// from, and the canonical [`ProjectSlug`] every world, vault, and `.localhost` hostname uses.
///
/// `vcs_root` is the main checkout root even when the request arrived from a linked
/// `.worktrees/<fork>` checkout (`ADR-0005` §3), while `worktree_root` is that linked checkout —
/// the tree whose dirty files a fork snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot {
    pub vcs_root: PathBuf,
    pub worktree_root: PathBuf,
    pub project: ProjectSlug,
}

/// One request to materialize a fork workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRequest {
    pub repo: RepoRoot,
    pub fork: String,
    /// Commit-ish to fork from (`None` forks the requesting checkout's `HEAD`).
    pub base: Option<String>,
}

/// The two refs every workspace exposes (`ADR-0005` §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRefs {
    /// Fork branch (`refs/heads/<fork>` in Git; `@-` projected as the same name in Jujutsu).
    pub base: String,
    /// Synthesized dirty view (`refs/slipfork/<fork>/wip`; equals the base commit when clean).
    pub wip: String,
}

/// Canonical fork branch refname (`refs/heads/<fork>`).
///
/// The snapshot ref deliberately lives outside `refs/heads/*`: Git's D/F refname lock forbids
/// `refs/heads/<fork>` and `refs/heads/<fork>/wip` from coexisting on every ref backend.
pub fn fork_branch_ref(fork: &str) -> String {
    format!("refs/heads/{fork}")
}

/// Canonical dirty-view refname (`refs/slipfork/<fork>/wip`).
pub fn fork_snapshot_ref(fork: &str) -> String {
    format!("refs/slipfork/{fork}/wip")
}

/// The dirty view of one workspace relative to its fork branch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceChanges {
    /// Modified, deleted, type-changed, or renamed paths tracked by the fork branch.
    pub tracked: Vec<PathBuf>,
    /// Untracked (non-ignored) paths.
    pub untracked: Vec<PathBuf>,
    /// `true` when more than [`MAX_WORKSPACE_CHANGES`] paths were present.
    pub truncated: bool,
}

/// Injected `<vcs_root>/slipfork.toml` `project = "<slug>"` reader.
///
/// `slipforkd` implements this once over `slipfork_dag::SlipforkConfig`, so `crates/slipfork-vcs`
/// honors the override without depending on `slipfork-dag` or owning a second TOML parser.
pub trait ProjectSlugOverride: Send + Sync {
    /// Return the override declared in `<vcs_root>/slipfork.toml`, or `None` when absent.
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError>;
}

/// Unit [`ProjectSlugOverride`] for repositories without `slipfork.toml`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoProjectOverride;

impl ProjectSlugOverride for NoProjectOverride {
    fn project_override(&self, _vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        Ok(None)
    }
}

/// Resolve a start directory to its canonical repository identity.
pub trait RepoRootResolver: Send + Sync {
    fn resolve(&self, start: &Path) -> Result<RepoRoot, VcsError>;
}

/// Materialize and tear down one fork workspace.
pub trait VcsWorkspace: Send + Sync {
    fn fork_name(&self) -> &str;
    fn path(&self) -> &Path;
    /// The two refs every adapter exposes for [`VcsWorkspace::fork_name`].
    ///
    /// `refs/heads/<fork>` is the committed branch; `refs/slipfork/<fork>/wip` is the dirty view,
    /// which deliberately lives outside `refs/heads/*` because Git's D/F refname lock forbids it
    /// from coexisting with `refs/heads/<fork>` (`ADR-0005` §3). Both adapters share this canonical
    /// layout, so the mapping is defined once here instead of being re-implemented per backend.
    fn refs(&self) -> WorkspaceRefs {
        WorkspaceRefs {
            base: fork_branch_ref(self.fork_name()),
            wip: fork_snapshot_ref(self.fork_name()),
        }
    }
    /// Resolve one of [`VcsWorkspace::refs`] to the commit object id consumers fetch.
    ///
    /// `git-remote-slipfork` advertises these ids from `list` and verifies every `fetch` request
    /// against them, so the ids an adapter reports and the objects Git receives can never diverge.
    fn resolve(&self, reference: &str) -> Result<String, VcsError>;
    /// Live dirty view of the checkout relative to [`WorkspaceRefs::base`].
    fn changes(&self) -> Result<WorkspaceChanges, VcsError>;
    /// Idempotent teardown: removes the checkout, the fork branch, and the snapshot ref.
    fn forget(&self) -> Result<(), VcsError>;
}

/// Backend-selecting factory held by `slipforkd`; the only place a backend kind is chosen.
pub trait VcsWorkspaceFactory: Send + Sync {
    fn create_workspace(
        &self,
        request: &WorkspaceRequest,
    ) -> Result<Box<dyn VcsWorkspace>, VcsError>;
}
