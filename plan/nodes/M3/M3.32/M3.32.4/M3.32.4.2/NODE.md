---
id: M3.32.4.2
title: "Command behaviours: env, run, per-service shell/exec, piped db/redis, secret set, 3-row doctor, drop guard, open"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.4.1]
paths:
  - crates/slipfork/src/cli.rs
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/logs.rs
  - crates/slipfork/src/open.rs
  - crates/slipfork/src/restart.rs
  - "crates/slipfork/src/daemon/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T21:01"
updated: "2026-11-19T21:01"
---

# Goal
Ship the remaining §2 behaviours: `env [-s] [--reveal]` replacing `resolve`; `run <task> [args...]`; per-service `shell [svc]` with a `(slipfork:<world>/<svc>)` prompt; `exec [-s <svc>] <cmd...>` with cwd-aware service auto-selection; piped non-TTY `db`/`redis`; hidden TTY prompt on `secret set`; 3-row verdict-first `doctor` (`✓ Ready` / `✗ N issue(s)`); `drop` dirty-worktree guard with `-f`; inline last-15-lines hook failure citing `Forkfile:<line>`; `$SSH_CONNECTION` `open` printing the `ssh -N -L` line.

# Context
`docs/design/dx-cli-0.1.0.md` §2-§3; `crates/slipfork/src/commands/{inspect,control,process,boot,doctor,secret,forward,daemon}.rs`; `src/open.rs`; `src/restart.rs`; `src/logs.rs`.

# Acceptance
- `cargo test -p slipfork -p slipfork-wire --all-targets` exits 0; clippy `-D warnings`; fmt clean.
- `! grep -rn "snow resolve" crates/slipfork/src` exits 0.
- `--json` shapes byte-match §3 for `up`, `logs`, `env`, `open`, `doctor`, `secret list`, `prune`.
- Write NO `RESULT.md`.
