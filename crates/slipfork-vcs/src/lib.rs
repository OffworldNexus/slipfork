//! `slipfork-vcs`: version-control workspace ports and their Git / Jujutsu adapters.
//!
//! `slipforkd` wires these ports and passes them into world orchestration; domain crates never shell
//! out, never read `std::env`, and never branch on a backend kind:
//!
//! - `ProjectSlugOverride`: injected `<vcs_root>/slipfork.toml` `project = "<slug>"` reader,
//!   implemented once at the composition root over `slipfork_dag::SlipforkConfig` (this crate
//!   carries zero `slipfork-dag` dependency and zero second TOML parser).
//! - `RepoRootResolver`: resolves a start directory to `RepoRoot` (worktree root, VCS root,
//!   canonical `ProjectSlug`), failing closed with `VcsError::NotARepository` /
//!   `VcsError::UnsupportedTopology`.
//! - `VcsWorkspaceFactory` / `VcsWorkspace`: create and tear down `.worktrees/<fork>` checkouts that
//!   expose `WorkspaceRefs` (`refs/heads/<fork>` plus the synthesized `refs/slipfork/<fork>/wip`
//!   snapshot), report their `WorkspaceChanges`, and answer `current_branch` for zero-argument
//!   `slipfork fork` (whose branch name is turned into a fork name by [`sanitize_fork_name`]). Two
//!   adapters implement the identical contract:
//!   `GitWorktreeAdapter` (linked Git worktree plus a scratch-index snapshot) and
//!   `JjWorkspaceAdapter` (Jujutsu workspace projecting `@-` / `@` into the colocated Git store).
//!   Fork repositories must keep `.worktrees/` and `.slipfork/` out of version control: Jujutsu
//!   snapshots every non-ignored path into the working-copy commit, exactly like a Git fork
//!   snapshot would otherwise fold sibling checkouts into itself.
//! - `remote_helper`: the pure `git-remote-slipfork` line protocol (`capabilities`, `list`, `fetch`,
//!   `push`, `option`) driven by the `git-remote-slipfork` executable in `crates/slipfork`.
//!
//! Adapter modules own every `std::process::Command` invocation; [`ports`] and [`remote_helper`]
//! stay free of process, environment, and clock access. The crate ships **zero** `src/bin/*`
//! targets: the `git-remote-slipfork` executable lives in `crates/slipfork` and drives the pure
//! [`remote_helper`] line protocol.

#![forbid(unsafe_code)]

mod git_store;
mod tool_command;

pub mod git;
pub mod jj;
pub mod ports;
pub mod remote_helper;
pub mod repo_root;

pub use git::GitWorktreeAdapter;
pub use jj::JjWorkspaceAdapter;
pub use ports::{
    MAX_WORKSPACE_CHANGES, NoProjectOverride, ProjectSlugOverride, RepoRoot, RepoRootResolver,
    VcsError, VcsWorkspace, VcsWorkspaceFactory, WORKTREES_DIRECTORY, WorkspaceChanges,
    WorkspaceRefs, WorkspaceRequest, sanitize_fork_name,
};
pub use remote_helper::{MAX_FRAME_BYTES, RemoteHelperError};
pub use repo_root::GitRepoRootResolver;
