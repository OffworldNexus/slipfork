---
id: M3.40.10
title: "Streamed up/restart/down phases, failure tails, ANSI matrix and spec convergence sweep"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.40.9]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T14:29"
updated: "2026-11-21T14:52"
---

# Goal
Finish the `specs/02-cli-ux.md` §2/§5 contract: streamed `up`/`restart`/`down` phases, inline failure tails, a machine-checked ANSI-purity matrix across every verb, and the final spec/docs convergence sweep.

# Context
- `up`/`down`/`restart` already stream typed `event` frames from the daemon; this node renders `[step/total]` phase headers with elapsed, `(up to date)` badges for stamped skips, and an inline red failure tail with `Forkfile:<line>` plus exactly one `next:`.
- A failed service must keep the world running and the daemon entry live.
- Every shipped verb and flag lives in `crates/slipfork/src/cli/surface.rs`; the matrix test iterates that table instead of a hand-copied list.

# Acceptance
- `cargo test -p slipfork`: one table-driven journey runs every verb with `NO_COLOR=1`, `TERM=dumb`, a piped stdout, and `--json`, asserting zero `\x1b[` and a parseable JSON object where the spec defines one.
- A live (`required-features = ["live"]`) journey asserts the `[step/total]` headers, an `(up to date)` badge on the second boot, and a failure tail containing `Forkfile:<line>` + one `next:` while `slipfork status` still shows the world.
- `specs/02`/`specs/03`/`TESTING_PLAN.md` match the shipped surface 1:1 (no promise without behaviour), `<= 2`-sentence paragraphs.
- `cargo clippy -p slipfork --all-targets -- -D warnings`; `critical_guard.py`; `guard.py`.
