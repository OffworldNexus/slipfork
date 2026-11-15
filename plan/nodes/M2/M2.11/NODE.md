---
id: M2.11
title: "slipfork-vcs: RepoRootResolver/VcsWorkspace ports plus GitWorktreeAdapter and conformance"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.3]
paths: ["crates/slipfork-vcs/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Land the version-control ports in `crates/slipfork-vcs` (`RepoRootResolver`, `VcsWorkspace`, `VcsWorkspaceFactory`) plus the first real adapter, `GitWorktreeAdapter`, which creates `.worktrees/<fork>` and synthesizes the `<fork>/wip` commit from a scratch index without ever touching the live repository index.

# Context

- `ADR-0008` (locked by `M2.1`) fixes the DI shape: domain code asks a `VcsWorkspaceFactory` for a `VcsWorkspace`; it never shells out, never reads `std::env`, and never branches on a backend kind.
- The problem being solved: an agent opens a worktree while the developer has uncommitted work. A plain `git worktree add` checks out `HEAD`, so the agent would not see that work. Slipfork snapshots the dirty tree into a throwaway `<fork>/wip` ref computed through a temporary `GIT_INDEX_FILE` under `.slipfork/`, leaving the developer index untouched.
- `M2.2` already declared the crate manifest, the workspace dependencies, and the `slipfork-world` edge; this node touches only `crates/slipfork-vcs/**`.
- Worktrees live on disk under `.worktrees/` inside the repository, never under `/tmp` (tmpfs).

# Design / Invariants

1. **Ports.**
   - `RepoRootResolver { fn resolve(&self, start: &Path) -> Result<RepoRoot, VcsError> }` returns the worktree root plus the git common dir; the adapter walks parents looking for `.git` (file or directory) and fails closed with `VcsError::NotARepository { start }`.
   - `VcsWorkspace` exposes `fork_name() -> &str`, `path() -> &Path`, `refs() -> WorkspaceRefs`, `changes() -> Result<WorkspaceChanges, VcsError>` (tracked, untracked, truncated-diff flag) and `forget()` (idempotent teardown).
   - `VcsWorkspaceFactory { fn create_workspace(&self, request: &WorkspaceRequest) -> Result<Box<dyn VcsWorkspace>, VcsError> }` with `WorkspaceRequest { repo: RepoRoot, fork: ForkName, base: Option<RefName> }`.
2. **Git creation sequence.** `git worktree add --no-checkout <repo>/.worktrees/<fork> -b <fork> <base>`; then, with `GIT_INDEX_FILE` pointing at `.slipfork/git/index-<fork>`: `read-tree HEAD`, `add -A`, `write-tree`, `commit-tree <tree> -p <fork> -m slipfork: fork snapshot`, `update-ref refs/heads/<fork>/wip <commit>`, then check out the worktree. Every step is a `Command` argument vector, never a shell string.
3. **The live index is sacred.** The repository real `.git/index` is never opened, renamed, or locked by the synthesizer; the critical test asserts inode and mtime are unchanged across a fork of a dirty tree.
4. **Fail closed.** Existing branch or existing worktree path: `VcsError::WorkspaceExists { fork }` (never overwritten). Missing `git` binary: `VcsError::ToolMissing { tool }`. Unreadable repository: `VcsError::NotARepository`. No silent fallback to a file copy and no best-effort staging.
5. **Protected conformance.** `crates/slipfork-vcs/tests/critical_vcs.rs` ships `assert_vcs_workspace_conformance(factory)` covering clean fork, dirty fork with untracked and modified files, base-ref ancestry, `forget()` idempotency, and the live-index-untouched assertion, so `M2.12` runs the identical matrix against the second adapter.
6. `#![forbid(unsafe_code)]`; ambient access (current dir, env, clock, subprocess) is confined to adapter modules; the ports module has zero `std::process`.

# Acceptance

```bash
# 1. Ports and the Git adapter satisfy the shared conformance matrix.
cargo test -p slipfork-vcs --all-targets
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture git_conformance
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture dirty_fork
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture index_untouched

# 2. Fail-closed paths (repeat fork, missing tool, non-repository).
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture fail_closed

# 3. Purity and lint gates.
! rg -q "std::process" crates/slipfork-vcs/src/ports.rs
cargo clippy -p slipfork-vcs --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a second `create_workspace` for the same fork is refused rather than overwritten, `.worktrees/<fork>` exists inside the repository and never under `/tmp`, `refs/heads/<fork>/wip` equals the base when the tree is clean and carries the dirty tree when it is not, and the live `.git/index` inode plus mtime are unchanged.
