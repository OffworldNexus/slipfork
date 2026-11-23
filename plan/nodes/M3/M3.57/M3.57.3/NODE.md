---
id: M3.57.3
title: "plan frame at t=0, precomputed widths, live spinner rows & DECSTBM summary"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.57.2]
paths:
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork-world/src/boot/**"
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork/src/daemon/**"
  - "specs/02-cli-ux.md"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T22:37"
updated: "2026-11-22T23:09"
---

# Goal
Supervisor emits a `plan` frame before any step; CLI computes ColumnWidths upfront; BootTui allocates per-task spinner rows on Started and a `N running (...)` DECSTBM bar.

# Acceptance
- `plan` precedes first Started and lists every step/pod; widths stable rows 1..N.
- 2D VT100 prompt-safety tests stay green; specs/02 + --json documented.
