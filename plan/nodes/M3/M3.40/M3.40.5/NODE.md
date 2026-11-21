---
id: M3.40.5
title: Remaining CLI verbs (fork/drop/logs/shell/exec/db/redis/env/secret/open/auth/completions) + terminal hygiene
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.4]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "specs/02-cli-ux.md"
  - TESTING_PLAN.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T12:50"
---

# Goal
Complete the `specs/02-cli-ux.md` daily surface and enforce terminal hygiene across every verb.

# Context
- Verb table: `crates/slipfork/src/cli/parse.rs:41-48`; help topics `crates/slipfork/src/cli/help.rs:23` (+ per-topic renderers); dispatch `crates/slipfork/src/cli/mod.rs`; hand-written verbs in `crates/slipfork/src/commands/mod.rs` (`Session::fmt`/`migrate` show the error shape: 1 line + `next:`).
- Add: `fork [name]` (snapshot summary), `drop <name> [-f]`, `logs [-f] [service]` (colored gutters, chronological merge, `── restarted ──` divider across restarts), `shell [service|image]`, `exec [service|image] -- <cmd>`, `db [database]`, `redis [id]`, `env [service] [--reveal]`, `secret list|set|unset`, `open [service]` (SSH/headless-safe), `auth [<endpoint-url>]` (drives `.2`'s OSPP client), `completions <shell>`.
- Upgrade `up`/`restart [service]`/`down`: streamed `[step/total]` phases with spinners + elapsed, `(up to date)` badges, inline red failure tail with `Forkfile:<line>` and `next:`, keep failed worlds running.
- Colors per `specs/02`; zero `\x1b[` under `NO_COLOR`, `TERM=dumb`, piped stdout, or `--json`. Never swallow >200ms child work in `.output()` - `spawn()` and stream.
- Talk to the daemon through the NDJSON client from `.4`; do not add a second socket implementation.

# Acceptance
- `cargo test -p slipfork` + `cargo test -p slipfork --test critical_cli_surface`: every new verb has help text, an accepted `--json` shape where the spec defines one, and an error path with `next:`.
- ANSI purity matrix: each verb run with `NO_COLOR=1`, `TERM=dumb`, and piped stdout asserts no `\x1b[` and no spinner bytes.
- `fork`/`drop` E2E against a real daemon + CoW world reports elapsed time and leaves no leftover subvolume.
- `cargo clippy -p slipfork --all-targets -- -D warnings`.
- `uv run --no-project tools/critical_guard.py`.
