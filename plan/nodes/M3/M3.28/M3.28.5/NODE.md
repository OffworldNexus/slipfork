---
id: M3.28.5
title: "VCS: branch-named fork source, pinned slipfork snapshot identity, and one-line git errors"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.2]
paths: ["crates/slipfork-vcs/src/**", "crates/slipfork-vcs/tests/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T16:32"
---

# Goal

`slipfork fork` with no argument derives its name from the current git branch (sanitised, collision-checked), the internal snapshot commit carries an explicit `slipfork` identity so a missing user `git config user.email` can never surface, and every git failure renders as one `slipfork:` line instead of a raw git dump.

# Context

Fact 5, twice: `slipfork fork` demanded `<FORK>`, then `slipfork fork tralala` printed `git commit-tree … failed (128): Author identity unknown` and the owner had to set `user.email`/`user.name` by hand. `crates/slipfork-vcs/src/git.rs:182-196` runs `commit-tree` with no identity; `VcsWorkspaceFactory` (`src/ports.rs:192`) has no way to answer "which branch is this checkout on". Parallel with `M3.28.3`/`M3.28.4` (disjoint crates).

# Design / Invariants

1. Every snapshot commit passes the identity as git global options before the subcommand: `-c user.name=slipfork -c user.email=slipfork@localhost`. Never read, write, or require the user's git config; no `includeIf`, no environment fallback. Same rule in the jj adapter.
2. New `VcsWorkspaceFactory::current_branch(&self, checkout: &Path) -> Result<Option<String>, VcsError>`: `Some(branch)` for a git branch checkout and a jj booked working copy, `None` for detached HEAD / bookmark-less jj (the caller then fails closed — never a generated name).
3. One sanitisation rule exported from this crate (lowercase, `/` → `-`, drop non `[a-z0-9-]`, reject empty) plus a collision check against the fork's existing refs/worktree, so `feat/ux-pass` becomes `feat-ux-pass` and `feat-ux-pass` twice fails closed.
4. Non-zero git/jj exits become one-line `VcsError` values (failing subcommand + last stderr line); no multi-line advice, no swallowed stderr, no `#[allow]`.
5. All spawning stays inside the existing `tool_command` runner; shared conformance suite covers both adapters.

# Acceptance

```console
$ GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo test -p slipfork-vcs --test critical_vcs
... test result: ok
$ cargo test -p slipfork-vcs --test critical_conformance_matrix
... test result: ok
$ cargo test -p slipfork-vcs --lib
... test result: ok
$ grep -q 'user\.name=slipfork' crates/slipfork-vcs/src/git.rs && echo identity-pinned
identity-pinned
$ ! grep -rn 'user\.name\|user\.email' crates/slipfork/src && echo cli-owns-no-identity
cli-owns-no-identity
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- The `GIT_CONFIG_GLOBAL=/dev/null` run is the proof: the snapshot commit succeeds with no user identity anywhere.
- A detached-HEAD fixture returns `Ok(None)`; a failing fixture yields a single-line error.
