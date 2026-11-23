---
id: M3.32.2.2.2
title: "slipfork-engine direct-execve lifecycle runner ([1/N], Forkfile:<line> fail-fast, zero sh -c)"
kind: epic
status: done
priority: 3
depends_on: [M3.32.2.2.1.3.2.1]
paths: ["crates/slipfork-engine/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:24"
updated: "2026-11-19T18:52"
---

# Goal

Run every lifecycle step through direct `execve` argv with live `[1/N]` progress and `Forkfile:<line>` fail-fast.

# Context

Split from `M3.32.2.2`; spec `docs/design/forkfile.md` section 3 and `docs/design/dx-cli-0.1.0.md` section 1 (setup/pre-deploy failure transcript).

- Consume `slipfork-forkfile::EvaluatedFork` (add workspace dep) for service hooks/tasks argv.
- `run` supervises exactly one child; `setup`/`pre-deploy`/`post-deploy`/`failed-deploy`/`task` argv lines execute sequentially, streaming `[1/N]` phase headers with elapsed time, failing fast and citing `Forkfile:<line>` plus the last 15 output lines.
- Secret-tainted argv slots never print (`[REDACTED]`).
- Delete the remaining `/bin/sh -c` in `crates/slipfork-engine/src/stream.rs`.

# Acceptance

- `cargo test -p slipfork-engine` exits 0, including a lab test that a failing hook reports `Forkfile:<line>` and stops the sequence.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- Zero `/bin/sh -c` or `sh -c` argv construction under `crates/slipfork-engine`.
- Write NO `RESULT.md`.
