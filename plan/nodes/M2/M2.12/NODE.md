---
id: M2.12
title: "slipfork-vcs: JjWorkspaceAdapter plus git-remote-slipfork protocol handler"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.11]
paths: ["crates/slipfork-vcs/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T18:39"
---

# Goal

Add the second `VcsWorkspace` adapter, `JjWorkspaceAdapter` (workspace add/forget with the `@` and `@-` ref projection), and the `git-remote-slipfork` line-protocol handler so a plain `git fetch slipfork` can materialize a fork world. Finish with both adapters passing the identical `assert_vcs_workspace_conformance` matrix.

# Context

- `M2.11` landed the ports, `GitWorktreeAdapter`, and `critical_vcs.rs` with its conformance entry point; this node extends the same files.
- Jujutsu has no branches: the working copy is the `@` commit and its parent is `@-`. A fork world therefore maps `@` to `<fork>/wip` (the dirty snapshot) and `@-` to `<fork>` (the base), so the CLI can show the same two refs for both back ends.
- `git-remote-slipfork` is a git remote helper: git spawns it with the remote name and speaks the documented line protocol over stdin and stdout.
- `jj` may be absent on a host. The adapter must fail closed with an actionable diagnostic for users and skip **loudly** (never silently) in the hermetic conformance matrix.

# Design / Invariants

1. **Jj mapping.** `create_workspace`: `jj workspace add <repo>/.worktrees/<fork> --name <fork> --revision <base>`; the snapshot is produced inside that workspace (`jj new`, then bookmark `<fork>/wip` at the resulting change). `forget()` runs `jj workspace forget <fork>` and is idempotent.
2. **Ref projection** is one small function per adapter returning `WorkspaceRefs { base, wip }`. Neither the CLI nor the remote helper branches on adapter kind (`AdapterKind` matches outside the factory are rejected in review).
3. **Missing-tool policy.** `JjWorkspaceAdapter::detect()` returns `VcsError::ToolMissing { tool: "jj" }`. The conformance runner prints exactly one explicit `skipped: ToolMissing(jj)` line for the Jj family and still fails the run if the Git family skipped; a runtime `create_workspace` call always fails closed, with no fallback to Git.
4. **`git-remote-slipfork` handler** (pure library in `crates/slipfork-vcs/src/remote_helper.rs`): `capabilities` -> `fetch`, `push`, `option`; `list` -> one `<sha>` TAB `<ref>` line per fork ref (`refs/heads/<fork>` and `refs/heads/<fork>/wip`); `fetch <sha> <name>` -> verifies/exports ref via `VcsWorkspace` and terminates batch with blank line; `push` -> `error <ref> <reason>` for every ref (worlds are fetched, not pushed over remote-helper in `0.2.0`). Frames are size-bounded and an unknown verb answers `unsupported` without panicking.
5. **Single Composition Root preserved (`Engineering Law #2`, `H-010` #1):** `crates/slipfork-vcs` ships **zero** `src/bin/*` binaries; the `git-remote-slipfork` executable lives solely in `crates/slipfork` (`M2.17`) and delegates into `slipfork_vcs::remote_helper`.
6. `#![forbid(unsafe_code)]`; the protocol handler takes injected `BufRead`/`Write` + `VcsWorkspace` port with zero direct filesystem or subprocess access.

# Acceptance

```bash
# 1. Both adapters pass the same conformance matrix.
cargo test -p slipfork-vcs --all-targets
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture conformance_matrix
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture jj_conformance
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture jj_absent

# 2. Remote helper protocol (list, fetch, push refusal, unknown verb).
cargo test -p slipfork-vcs --test critical_vcs -- --nocapture remote_helper

# 3. No adapter-kind ladder outside the factory and no duplicate binary target.
! test -d crates/slipfork-vcs/src/bin
! rg -q "AdapterKind" crates/slipfork-vcs/src/ports.rs crates/slipfork-vcs/src/remote_helper.rs
cargo clippy -p slipfork-vcs --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: an unknown protocol verb yields `unsupported` and keeps the process alive, every `push` is refused with a reason, a missing `jj` binary produces exactly one explicit skip line plus a fail-closed runtime error, and the two refs of a dirty Jj world resolve to distinct commits.
