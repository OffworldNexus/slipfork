---
id: M3.32.8
title: "slipfork migrate CLI (file/output/--stdout/--force, crisp summary) + dx-cli-0.1.0 spec row"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.4, M3.32.7]
paths: ["crates/slipfork/**", "docs/design/dx-cli-0.1.0.md", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T16:00"
updated: "2026-11-19T16:00"
---

# Goal

Ship `slipfork migrate [FILE] [-o|--output <PATH>] [--stdout] [-f|--force]` on the clean-sheet CLI.

# Context

OWNER ADDITION to `M3.32`. Depends on the converter (`M3.32.7`) and the final CLI surface (`M3.32.4`).

- Defaults `FILE` to `./snow.yml` (or `./snow.yaml`) and output to `./Forkfile`; refuse to overwrite an existing `Forkfile` unless `-f/--force`; `--stdout` prints without writing.
- Print `✓ Migrated snow.yml → Forkfile (2 services, 1 postgres cluster, 3 env groups)` (dynamic counts, ANSI-free under `NO_COLOR`/non-TTY).
- Failures follow `error: <1 line>\n  next: <remedy>` and `--json` is `{"ok":false,"code","error","next"}`.
- Add the `migrate` row to `docs/design/dx-cli-0.1.0.md` section 3 (command, purpose, `--json` document `{"ok":true,"input","output","services","pg_clusters","env_groups"}` plus `--stdout` string mode).

# Acceptance

- `cargo test -p slipfork` exits 0: PTY/`--json` shape tests, refusal-without-`--force`, `--stdout`, missing file, unknown YAML key, and a realistic fixture converting cleanly.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
