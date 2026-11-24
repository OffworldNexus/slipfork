---
id: M3.107.5
title: "Bare `logs -r` replays the latest failed run"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.107.4]
paths:
  - crates/slipfork/src/cli/verbs.rs
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/daemon/lifecycle/replay.rs
  - "packaging/man/**"
  - docs/cli.md
  - docs/troubleshooting.md
  - "specs/02-cli-ux.md"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T12:55"
updated: "2026-11-24T12:55"
---

# Goal
`slipfork logs -r` with no value answers "what just broke": it replays the latest failed run.

# Context
- `crates/slipfork/src/cli/verbs.rs:171` `#[arg(short = 'r', long = "run", value_name = "RUN", conflicts_with = "runs")]` requires a value today.
- Daemon `resolve_run` (daemon/lifecycle/logs.rs) already understands `failed` / `last`.
- `logs` takes positional targets: a bare `-r api` would bind `api` as the RUN value — document `-r=<id>` / `--run <id>` and keep `logs api -r` unambiguous.

# Acceptance
- CLI: `-r, --run [RUN]` with `num_args(0..=1)`, `default_missing_value = "failed"`; `-r` → latest failed, `-r last` → newest run, `-r <id>` unchanged; `--help` lists `failed` (default), `last`, `<id>`.
- No failed run: one line + `next: run \`slipfork up\` or \`slipfork logs --runs\``, exit non-zero, never an empty replay; `--json` emits a typed error object.
- Man page regenerated (`cargo run -p slipfork -- --manpage > packaging/man/slipfork.1` or the repo's existing man generator); `docs/cli.md` + `specs/02-cli-ux.md` `logs` lines updated (shipped behaviour only).
- Live journey (extend, no new boot): after the failing `RUN`, bare `logs -r` returns that run's id/header; `-r last` returns the newest record; `-r <id>` still works; `--json` mirrors each.
- fmt, clippy -D warnings, `tools/check_docs_honesty.py`, targeted tests; lead runs VM live.
