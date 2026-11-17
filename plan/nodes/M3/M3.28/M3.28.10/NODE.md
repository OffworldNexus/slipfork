---
id: M3.28.10
title: "slipfork fork with no argument: branch-derived name, worktree/origin/cd output, one-line git errors"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.9]
paths: [crates/slipfork/src/cli.rs, crates/slipfork/src/main.rs, crates/slipfork/tests/critical_cli.rs]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-17T16:33"
---

# Goal

Make `slipfork fork` work with no argument (delegating to the branch-derived naming from `M3.28.5`), print the worktree path, the origin URL and a copy-pasteable `cd` line, and render VCS failures as one-line `slipfork:` errors.

# Context

Fact 5: `slipfork fork` demanded `<FORK>`, then `slipfork fork tralala` dumped raw git (`Author identity unknown`) and the owner had to set `git config --global` by hand. `M3.28.5` ships `VcsWorkspaceFactory::current_branch`, the pinned `slipfork` snapshot identity, the sanitisation/collision rule, and single-line `VcsError`s; `M3.28.8` ships the actionable output block.

# Design / Invariants

1. `cli.rs`: `Fork.fork: Option<String>` with help `Fork name (default: the current branch)`; `drop` keeps its required name. `ForkRequest.fork` becomes `Option<String>` and `slipforkd` resolves it through `M3.28.5` (the CLI never runs git, never reads `.git`).
2. Fail-closed cases, one line each, naming the next command: detached HEAD → `slipfork fork <name>`; already inside `.worktrees/<fork>` → run from the repository root; existing fork → `slipfork up --world <name>`; sanitised name equals the project name.
3. Success prints: `[world] <project>:<fork>`, `[worktree] <abs path>`, `[origin] <url>` (port-less at `:80`), `cd <abs path>`. `--json` carries `world`, `worktree`, `origin`, `changed`.
4. Every git/jj failure surfaces as a single `slipfork: <subcommand> failed (exit <n>): <last stderr line>` — no git advice blocks, no multi-line passthrough, no `hub`/`help` links.
5. `--base`, dump and resource flags keep working unchanged; no behaviour duplicated from the VCS crate.

# Acceptance

```console
$ cargo run -q -p slipfork -- fork --help | grep -qF '[FORK]' && echo optional-fork
optional-fork
$ cargo test -p slipfork --lib fork
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ ! grep -rn 'commit-tree\|rev-parse' crates/slipfork/src && echo no-git-in-cli
no-git-in-cli
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_cli` covers: branch-named fork with `GIT_CONFIG_GLOBAL=/dev/null` (no user identity available), `feat/ux-pass` → `feat-ux-pass`, detached HEAD → exit 1 naming `slipfork fork <name>`, and an existing `.worktrees/<branch>` → exit 1 naming `slipfork up --world <branch>`.
- `fork --help` shows `[FORK]`, never `<FORK>`.
