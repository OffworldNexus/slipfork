---
id: M3.173
title: "Slice 2c: boot pane on ui/report/live.rs (one status line, rows from Report), delete the bespoke renderer"
kind: epic
status: superseded
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/ui/failure.rs
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/tests/boot_pty.rs
  - crates/slipfork/tests/ansi_matrix.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T17:06"
updated: "2026-11-25T17:25"
---

# Goal (Report layer slice 2c, after 2b)

Replace the bespoke boot pane with `ui/report/live.rs` (`Pane` + one in-place status line, `O(active+8)` rows), rows built from `Report`/`Row`/`StepSettlement`. Delete `boot/failure.rs`, `Node`, `ImageEnd`/`ImageState`, `ImageTally::settle()`'s planned-count path and the dual `lifecycle_failure`. Rewrite the boot tests against `Report`.

Preserve the shipped append-only behaviour exactly: ordinary newline rows, one status line (`\r` + `ESC[2K`) with spinner/label/elapsed and `· sealing`/`· publishing` tails, one verdict per instruction in plan order, padded counters, `cached N · built M`, zero escapes under `NO_COLOR`/`--json`/pipes, no DECSTBM/absolute addressing/clears/alt-screen.

# Acceptance

- PTY captures (110×30, 80×24) for cached/fresh/failing/streaming/resize are byte-identical to HEAD before the slice (except intentionally replaced internals); `ansi_matrix` and `boot_pty` green; protected `critical_status_live` untouched.
- `rg 'Node\b|ImageEnd|ImageTally' crates/slipfork/src/boot` empty; every boot file ≤500 LOC.
- `make check`, Triad (all three); commit `[M3.173] ...`, done, no push.
