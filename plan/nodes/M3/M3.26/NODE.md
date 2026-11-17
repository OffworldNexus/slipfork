---
id: M3.26
title: slipfork fork fails closed when .gitignore ignores .worktrees/.slipfork
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.19]
paths: ["crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T14:21"
updated: "2026-11-17T14:21"
---

# Goal

Make `slipfork fork` work in any project whose `.gitignore` already ignores `.worktrees/` or `.slipfork/` — today it fails closed with a git error.

# Context (live evidence, M3.19 attempt 2 on the Btrfs VM)

`slipfork fork feat-1` exits 1 when the project's `.gitignore` contains `.worktrees/`:

```
$ git add -A -- . ':(exclude).slipfork' ':(exclude).worktrees'
fatal: Pathspec '.worktrees' is in .gitignore
```

Git refuses an explicitly named pathspec that is ignored. Ignoring `.worktrees/` is the documented, recommended setup for a repo that uses git worktrees, so this hits ordinary users on their first `fork`. The demo only worked because it had no `.gitignore`.

# Design / Invariants

1. Stop naming ignored paths on the `git add` command line. Either stage with `git add -A -- .` after ensuring `.slipfork/` and `.worktrees/` are covered by `.git/info/exclude` (never by rewriting the user's `.gitignore`), or stage only the paths the fork actually touched.
2. Never modify the user's tracked `.gitignore`; never add a compatibility shim or a "try, else retry without excludes" fallback.
3. Keep Git and Jujutsu behaviour identical and keep the VCS adapter seam (`VcsWorkspace`) — this is an adapter-level fix.
4. Add a critical test: a repo whose `.gitignore` contains `.worktrees/` and `.slipfork/` must fork successfully and the fork must contain the committed tree without the excluded dirs.

# Acceptance

```bash
cargo test -p slipfork --test critical_cli fork
cargo test --workspace --lib
cargo clippy --workspace --all-targets -- -D warnings
make check
```
