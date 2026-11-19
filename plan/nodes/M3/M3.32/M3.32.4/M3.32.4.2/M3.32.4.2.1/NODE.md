---
id: M3.32.4.2.1
title: "Interaction verbs: env, run, per-service shell and exec"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.32.4.1]
paths: ["crates/slipfork/src/**", "crates/slipfork/tests/**", "crates/slipfork-wire/**"]
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-19T21:22"
updated: "2026-11-19T21:36"
---

# Goal
Ship the interaction verbs of `docs/design/dx-cli-0.1.0.md` §2: `env [-s <svc>] [--reveal] [-F ...]`, `run <task> [args...]`, per-service `shell [svc]`, and `exec [-s <svc>] <cmd...>`.

# Context
`crates/slipfork/src/commands/{inspect,control,process,boot}.rs`, `src/cli/commands.rs` (verb args), `src/daemon/{protocol,service,resolve}.rs`. Secrets render `[REDACTED]` (or `null` under `--json`) unless `--reveal` on a TTY. `shell` prompt is `(slipfork:<world>/<svc>)` with zero stderr lecture. `exec` accepts optional `--` and auto-selects the service from cwd. `run` streams the task argv live and propagates its exit code; an unknown task fails closed with `next:`.

# Acceptance
- `cargo test -p slipfork --lib --bins` exits 0; clippy `-D warnings`; fmt clean.
- `--json` shapes for `env`/`run`/`shell`/`exec` byte-match §3; failures are `{"ok":false,"code","error","next"}`.
- ANSI purity under `NO_COLOR`/pipe/`--json`; live streamed output for `run`.
- Write NO `RESULT.md`.
