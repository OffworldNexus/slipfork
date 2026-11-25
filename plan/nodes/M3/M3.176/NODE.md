---
id: M3.176
title: "Slice 2c/S3: boot rows from Report/Row, ImageTally cleanup, typed boot tests"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.175]
paths:
  - "crates/slipfork/src/ui/report/**"
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/boot_pty.rs
  - crates/slipfork/tests/ansi_matrix.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T17:25"
updated: "2026-11-25T18:02"
---

# Goal

Boot rows come from `Report`/`Row`/`StepSettlement`: add the `Row`/`Done` variants for the up-to-date (`state:"declined"`) and unknown-image (`• … finished`) rows — keep `finished` as an **explicit `Done::Finished`** (byte-preserving, not a fallback) — introduce `completed_row(&Row)`, rename `ImageTally`, delete `settle()`'s planned-count path, rewrite boot tests against `Report`, and fix `tests/status_surface.rs`'s 12 `Node` constructions.

# Acceptance

- HEAD-vs-after PTY diff (110×30, 80×24: cached/fresh/failing/streaming/resize) byte-identical except intended internals.
- `rg 'Node\b|ImageEnd|ImageTally' crates/slipfork/src/boot` empty; every boot file ≤500 LOC; `boot_pty`/`ansi_matrix`/`status_surface` green; protected `critical_status_live` untouched.
- `make check`, Triad (all three); commit `[M3.176] ...`, done, no push.
