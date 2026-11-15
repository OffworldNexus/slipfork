//! `slipfork-vcs`: version-control workspace ports and their Git / Jujutsu adapters.
//!
//! `slipforkd` wires these ports and passes them into world orchestration; domain crates never shell
//! out, never read `std::env`, and never branch on a backend kind:
//!
//! - `RepoRootResolver`: resolves a start directory to `RepoRoot` (worktree root, VCS common dir,
//!   canonical `ProjectSlug`), failing closed with `VcsError::NotARepository`.
//! - `VcsWorkspaceFactory` / `VcsWorkspace`: create and tear down `.worktrees/<fork>` checkouts that
//!   expose `WorkspaceRefs` (`<fork>` plus the synthesized `<fork>/wip` snapshot) and
//!   `WorkspaceChanges`.
//!
//! Adapter modules own every `std::process::Command` invocation; [`ports`] stays free of process,
//! environment, and clock access. The crate ships **zero** `src/bin/*` targets: the
//! `git-remote-slipfork` executable lives in `crates/slipfork` and drives the pure
//! [`remote_helper`] line protocol.

#![forbid(unsafe_code)]

pub mod git;
pub mod jj;
pub mod ports;
pub mod remote_helper;
pub mod repo_root;
