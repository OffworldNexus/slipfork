---
id: M3.32.4.2.2
title: "Ops verbs: piped db/redis, secret set, 3-row doctor, drop guard, open, hook failure tail"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.32.4.2.1]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-wire/**"
  - "docs/design/dx-cli-0.1.0.md"
attempts: 5
max_attempts: 6
escalation: none
created: "2026-11-19T21:22"
updated: "2026-11-19T21:48"
---

# Goal
Ship the remaining `docs/design/dx-cli-0.1.0.md` §2 verbs: piped `db`/`redis`, hidden TTY `secret set` prompt, 3-row verdict-first `doctor`, `drop` dirty-worktree guard, `$SSH_CONNECTION` `open`, and inline last-15-lines hook failure citing `Forkfile:<line>`.

# Context
`crates/slipfork/src/commands/{process,secret,doctor,forward,inspect}.rs`, `src/open.rs`, `src/restart.rs`, `src/cli/commands.rs`. Piped `db`/`redis` emit a clean URI on stdout with no spinners/ANSI; `doctor` leads `✓ Ready` / `✗ N issue(s)` and hides `[resources]` unless `-v`; `drop` refuses a dirty fork unless `-f` (one line + `next:`); `open` prints the `ssh -N -L` line and omits `:80`.

# Acceptance
- `cargo test -p slipfork -p slipfork-wire --all-targets` exits 0; clippy `-D warnings`; fmt clean.
- `--json` shapes for `db`/`redis`/`secret list`/`doctor`/`drop`/`open` byte-match §3.
- ANSI purity under `NO_COLOR`/pipe/`--json`; `secret set` never echoes the value.
- Write NO `RESULT.md`.
