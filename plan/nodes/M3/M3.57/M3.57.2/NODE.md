---
id: M3.57.2
title: "One borderless ANSI-aware table engine across boot/doctor/env/secret/status"
kind: task
status: active
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/doctor/**"
  - "crates/slipfork/src/commands/**"
  - "crates/slipfork/src/status/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T22:37"
updated: "2026-11-22T22:37"
---

# Goal
Canonical `ui/table.rs` (Left/Right, gap, indent, ANSI-aware widths, precomputed ColumnWidths, zero borders); migrate boot/render, doctor, env, secret, status; BootTui 6 fixed columns.

# Acceptance
- Column-index identity test rows 1..9 (short pg, 28-char name, `up to date`).
- Zero ad-hoc pad helpers left; ANSI purity preserved.
