//! Domain ports: `ProjectSlugOverride`, `RepoRootResolver`, `VcsWorkspaceFactory`, `VcsWorkspace`,
//! and their `RepoRoot` / `WorkspaceRequest` / `WorkspaceRefs` / `WorkspaceChanges` value types,
//! plus the single branch-to-fork-name rule ([`sanitize_fork_name`]).
//!
//! Zero subprocess spawning, zero environment reads, zero clock reads: adapters own all ambient
//! access, and no backend-kind discriminator exists outside `slipforkd`'s factory. The factory is
//! therefore also where "which branch is this checkout on" is asked, so zero-argument
//! `slipfork fork` never needs a second VCS path.

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
/// current directory" arm, and no adapter swallows a Git diagnostic. A non-zero tool exit is one
/// line — the failing subcommand plus the tool's last stderr line (`docs/design/dx-0.1.0-ux.md`
/// §7.4) — never a raw multi-line advice dump.
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
        "cannot derive a project slug from repository root {vcs_root}: '{slug}' is not a single lowercase DNS label; set `project <slug>` in {hint}"
    )]
    InvalidProjectSlug {
        vcs_root: PathBuf,
        slug: String,
        hint: String,
    },
    #[error(
        "cannot derive a project slug from repository root {vcs_root}: the directory name is not valid UTF-8; set `project <slug>` in {hint}"
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
    #[error("{command} failed (exit {status}): {stderr}")]
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

/// Turn a branch name into the one fork name Slipfork derives from it.
///
/// Sanitisation owns exactly two things (`docs/design/dx-0.1.0-ux.md` §7.1): mapping a branch string
/// (`/` -> `-`, ASCII lowercase, repeated `-` collapsed, edge `-` stripped) and rejecting a branch
/// that leaves nothing behind. The label grammar itself — the `[a-z0-9-]` alphabet, the 63-byte
/// limit, and the no-edge-`-` rule — is [`slipfork_world::is_valid_dns_label`]'s, so there is one
/// charset table in the workspace; a branch that cannot be a label (for example one longer than 63
/// bytes) yields `None` rather than being truncated into a *different* fork. Reserved `test-*`
/// names, the project name, and `domains[].id` collisions stay
/// [`slipfork_world::validate_fork_name`]'s job in `slipforkd`, which reports them as collisions
/// instead of as a missing branch.
pub fn sanitize_fork_name(branch: &str) -> Option<String> {
    let mut candidate = String::with_capacity(branch.len());
    for byte in branch.bytes() {
        let mapped = char::from(if byte == b'/' {
            b'-'
        } else {
            byte.to_ascii_lowercase()
        });
        if !is_label_byte(mapped) {
            continue;
        }
        if mapped == '-' && (candidate.is_empty() || candidate.ends_with('-')) {
            continue;
        }
        candidate.push(mapped);
    }
    while candidate.ends_with('-') {
        candidate.pop();
    }
    slipfork_world::is_valid_dns_label(&candidate).then_some(candidate)
}

/// `true` when `byte` belongs to the label alphabet, asked of the alphabet's owner.
///
/// A label of the form `a<byte>a` satisfies the grammar exactly when `byte` is a lowercase letter, a
/// digit, or `-`; uppercase, `.`, `_`, and non-ASCII bytes all break it. Asking
/// [`slipfork_world::is_valid_dns_label`] this way keeps the charset in one crate instead of
/// restating it here.
fn is_label_byte(byte: char) -> bool {
    slipfork_world::is_valid_dns_label(&format!("a{byte}a"))
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

/// Injected `<vcs_root>/Forkfile` `project <slug>` reader.
///
/// `slipforkd` implements this once over `slipfork_forkfile::ForkFile`, so `crates/slipfork-vcs`
/// honors the override without depending on the `Forkfile` parser or owning a second one.
pub trait ProjectSlugOverride: Send + Sync {
    /// Return the override declared in `<vcs_root>/Forkfile`, or `None` when absent.
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError>;
}

/// Unit [`ProjectSlugOverride`] for repositories without a `Forkfile`.
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

    /// The repository's **verbatim** `remote.origin.url`, or `None` when it has no origin.
    ///
    /// OSPP v1 project identity is this string and only this string (`docs/design/ospp.md`): no
    /// client-side slug, hash, mode, or canonicalization is applied. A repository without an
    /// origin simply never opens a subscription.
    fn remote(&self, repo: &RepoRoot) -> Result<Option<String>, VcsError>;
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

    /// The branch `repo` is on, for zero-argument `slipfork fork`.
    ///
    /// Taking the resolved [`RepoRoot`] makes the contract structural rather than prose: the backend
    /// is chosen from `repo.vcs_root` (the same field [`VcsWorkspaceFactory::create_workspace`] routes
    /// on) and the probe runs in `repo.worktree_root`, so the process working directory and a
    /// subdirectory of the repository are both unrepresentable here. The caller sanitises the answer
    /// through [`sanitize_fork_name`].
    ///
    /// `Some(branch)` is a Git branch checkout or a Jujutsu working copy carrying exactly one local
    /// bookmark; `None` is a detached `HEAD` or a bookmark-less Jujutsu working copy, which the caller
    /// must render as a fail-closed `slipfork fork <name>` request — never as a generated name.
    fn current_branch(&self, repo: &RepoRoot) -> Result<Option<String>, VcsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitisation_maps_a_branch_name_to_one_dns_label() {
        for (branch, expected) in [
            ("main", Some("main")),
            ("feat/ux-pass", Some("feat-ux-pass")),
            ("FEAT/UX-PASS", Some("feat-ux-pass")),
            ("feat//ux---pass", Some("feat-ux-pass")),
            ("-feat-ux-", Some("feat-ux")),
            ("fix/hot_fix", Some("fix-hotfix")),
            ("release/1.2", Some("release-12")),
        ] {
            assert_eq!(
                sanitize_fork_name(branch).as_deref(),
                expected,
                "branch {branch:?} sanitises to {expected:?}"
            );
        }
    }

    #[test]
    fn sanitisation_rejects_a_branch_that_leaves_no_label() {
        for branch in ["", "/", "///", "---", "!!!", ".."] {
            assert_eq!(
                sanitize_fork_name(branch),
                None,
                "branch {branch:?} cannot name a fork; the caller fails closed"
            );
        }
    }

    #[test]
    fn sanitisation_leaves_the_label_grammar_to_slipfork_world() {
        // 63 bytes is a label; 64 is not. Sanitisation never truncates into a different fork, and it
        // never restates the length rule — `slipfork_world::is_valid_dns_label` decides.
        let longest = "a".repeat(63);
        assert_eq!(
            sanitize_fork_name(&longest).as_deref(),
            Some(longest.as_str())
        );
        assert_eq!(
            sanitize_fork_name(&"a".repeat(64)),
            None,
            "an over-long branch is rejected by the label grammar, not silently shortened"
        );

        // Reserved names are shaped like labels, so sanitisation keeps them: reporting the collision
        // is `validate_fork_name`'s job in `slipforkd`, never a silent rename.
        assert_eq!(
            sanitize_fork_name("test-7").as_deref(),
            Some("test-7"),
            "the reserved test-* prefix is a collision, not a sanitisation failure"
        );
    }
}
