---
id: M3.107.7
title: "Keyword role = magenta 35 on every surface + byte-exact role test"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.107.6]
paths:
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T14:35"
updated: "2026-11-24T14:35"
---

# Problem (owner screenshot)
"layout is good but the colours…": step keywords (`FROM`/`ADD`/`RUN`) render dim (`boot/phase.rs::head`, `ui/palette.rs::header_line`/`verdict`), the same dim as timings, so keyword, timing and command don't read as different roles.

# Contract
Parent `# Visual contract (v3.2)` role table: keyword/verb magenta `35` via new `Palette::verb` (step rows running+settled, `logs -r` header, verdicts); target bold `1`; command/args neutral; counters/timings/`(exit N)`/`next:` label/timestamps dim `2`; `┃` 33, `│` 2; `✗`/`failed` 31; `✓`/`ready`/`running` 32; `#id` bold; next command 36. No `palette.kind`/`label` for verbs/targets.

# Acceptance
- One byte-exact golden test pins: running step row, settled `✓` row, `✗` verdict, `logs -r` header, `next:` row — styled AND `NO_COLOR` (zero `\x1b[`).
- `critical_status_live.rs:1066` bare-`logs` assertion accepts the v3 stream gutter (`alpha │` OR `alpha ┃`; python http.server logs to stderr) — no weakening beyond the glyph.
- fmt, clippy -D warnings, `cargo test -p slipfork --lib --bins`; lead runs VM live + PTY `cat -v` capture.
