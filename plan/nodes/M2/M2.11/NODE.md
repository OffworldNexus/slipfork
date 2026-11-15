---
id: M2.11
title: "slipfork-vcs: RepoRootResolver/VcsWorkspace ports plus GitWorktreeAdapter and conformance"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M2.3]
paths: ["crates/slipfork-vcs/**"]
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T19:34"
---

# Goal

Land the version-control ports in `crates/slipfork-vcs` (`ProjectSlugOverride`, `RepoRootResolver`, `VcsWorkspace`, `VcsWorkspaceFactory`) plus the first real adapter, `GitWorktreeAdapter`, which creates `.worktrees/<fork>` on branch `refs/heads/<fork>` and synthesizes the D/F-collision-proof `refs/slipfork/<fork>/wip` snapshot from a scratch index without ever touching the live repository index.

# Context

- `ADR-0008` and `H-010` #1 fix the DI shape: `slipforkd` (the unified user daemon Composition Root) wires `ProjectSlugOverride` (over `slipfork_dag::SlipforkConfig`), `RepoRootResolver`, and `VcsWorkspaceFactory`; domain code never shells out, never reads `std::env`, never adds a second `slipfork.toml` parser, and never branches on a backend kind.
- **Git D/F refname fact (proved live with Git 2.47.3 across `files` and `reftable`):** `refs/heads/<fork>` and `refs/heads/<fork>/wip` cannot coexist (`'refs/heads/<fork>' exists`). Keeping the fork branch at `refs/heads/<fork>` and placing the dirty view at **`refs/slipfork/<fork>/wip`** (`git fetch slipfork '+refs/slipfork/<fork>/wip:refs/slipfork/<fork>/wip'`) eliminates the D/F collision cleanly. (Published `docs/index.md:56` and `README.md:58` stay untouched here and are swept in `M2.19`/`M2.23`.)
- `M2.2` already declared the crate manifest, the workspace dependencies, and the `slipfork-world` edge (zero `slipfork-dag` dependency); this node touches only `crates/slipfork-vcs/**`.
- Worktrees live on disk under `.worktrees/` inside the repository, never under `/tmp` (tmpfs).

# Design / Invariants

1. **Ports (`ARCHITECTURE.md` §3.5).**
   - `ProjectSlugOverride: Send + Sync { fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError>; }` supplies `<vcs_root>/slipfork.toml`'s optional `project = "<slug>"` override without `crates/slipfork-vcs` depending on `slipfork-dag` (`NoProjectOverride` unit impl provided for tests without `slipfork.toml`).
   - `RepoRootResolver: Send + Sync { fn resolve(&self, start: &Path) -> Result<RepoRoot, VcsError>; }` resolves `RepoRoot { vcs_root, worktree_root, project }` using injected `Arc<dyn ProjectSlugOverride>` (`ADR-0005` §3 precedence: override wins, else `<vcs_root>` basename parsed via `ProjectSlug::parse`; bare repos, submodules, and `--separate-git-dir` fail closed; non-repositories fail with `VcsError::NotARepository { start }`).
   - `VcsWorkspace` exposes `fork_name() -> &str`, `path() -> &Path`, `refs() -> WorkspaceRefs`, `changes() -> Result<WorkspaceChanges, VcsError>` (tracked, untracked, truncated-diff flag) and `forget()` (idempotent teardown, also deleting `refs/slipfork/<fork>/wip`).
   - `VcsWorkspaceFactory { fn create_workspace(&self, request: &WorkspaceRequest) -> Result<Box<dyn VcsWorkspace>, VcsError>; }` with `WorkspaceRequest { repo: RepoRoot, fork: String, base: Option<String> }`.
2. **Git creation sequence & clean-tree rule (`ADR-0005` §3).** Run `git worktree add --no-checkout <repo>/.worktrees/<fork> -b <fork> <base>` (creating `refs/heads/<fork>`). With `GIT_INDEX_FILE` pointing at `<vcs_root>/.slipfork/git/index-<fork>`: `read-tree <fork>`, `add -A`, `write-tree`:
   - **Clean tree:** when `<tree>` equals `refs/heads/<fork>`'s tree OID (zero tracked/untracked diffs), `update-ref refs/slipfork/<fork>/wip <base_commit_oid>` directly — so `refs/slipfork/<fork>/wip` equals `refs/heads/<fork>` when clean without creating an empty commit.
   - **Dirty tree:** `commit-tree <tree> -p <fork> -m "slipfork: fork snapshot"`, then `update-ref refs/slipfork/<fork>/wip <commit>`, and populate/check out `<repo>/.worktrees/<fork>` without touching `<vcs_root>/.git/index`. Every step is a `Command` argument vector, never a shell string.
3. **The live index is sacred.** The repository's real `.git/index` is never opened, renamed, or locked by the synthesizer; the critical test asserts inode and mtime are unchanged across a fork of a dirty tree.
4. **Fail closed.** Existing branch, existing `refs/slipfork/<fork>/wip`, or existing worktree path: `VcsError::WorkspaceExists { fork }` (never overwritten). Missing `git` binary: `VcsError::ToolMissing { tool }`. Unreadable repository: `VcsError::NotARepository`. No silent fallback to a file copy and no best-effort staging.
5. **Protected conformance.** `crates/slipfork-vcs/tests/critical_vcs.rs` ships `assert_vcs_workspace_conformance(factory)` covering clean fork (`wip == base`), dirty fork with untracked and modified files (`refs/heads/<fork>` and `refs/slipfork/<fork>/wip` coexisting with distinct commits), base-ref ancestry, `ProjectSlugOverride` precedence, `forget()` idempotency, and the live-index-untouched assertion, so `M2.12` runs the identical matrix against the second adapter.
6. `#![forbid(unsafe_code)]`; ambient access (current dir, env, clock, subprocess) is confined to adapter modules; the ports module has zero `std::process` and zero `slipfork-dag` dependency.

# Acceptance

```bash
# 1. Ports and the Git adapter satisfy the shared conformance matrix (refs/heads/<fork> + refs/slipfork/<fork>/wip).
cargo test -p slipfork-vcs --all-targets
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture git_conformance
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture dirty_fork
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture index_untouched

# 2. Fail-closed paths (repeat fork, missing tool, non-repository, ProjectSlugOverride error).
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture fail_closed

# 3. Purity and lint gates (no slipfork-dag dep, no std::process in ports).
! rg -q "std::process" crates/slipfork-vcs/src/ports.rs
! rg -q "slipfork-dag" crates/slipfork-vcs/Cargo.toml
cargo clippy -p slipfork-vcs --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a second `create_workspace` for the same fork is refused rather than overwritten, `.worktrees/<fork>` exists inside the repository and never under `/tmp`, `refs/heads/<fork>` and `refs/slipfork/<fork>/wip` coexist on Git, `refs/slipfork/<fork>/wip` equals `refs/heads/<fork>` when the tree is clean and carries the dirty snapshot commit when it is not, `ProjectSlugOverride` overrides directory-basename slug derivation, and the live `.git/index` inode plus mtime are unchanged.
