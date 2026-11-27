---
id: M3.107.6
title: Split boot/pane.rs and daemon/lifecycle/logs.rs into cohesive modules (no behaviour change)
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.107.4]
paths:
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/daemon/lifecycle/replay.rs
  - "crates/slipfork/src/daemon/lifecycle/logs/**"
  - crates/slipfork/src/daemon/lifecycle/mod.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T13:58"
updated: "2026-11-24T14:35"
---

# Goal
No file in `crates/slipfork/src/boot/` exceeds 500 LOC at the M3.107 gate; pure refactor, zero behaviour change.

# Context
- `boot/pane.rs` is 672 LOC after M3.107.3 (DECSTBM region + task rows/selector + narrative block paint + tests). `boot/render.rs` 405, `boot.rs` ~498 (do not grow).
- Split along real seams, e.g. `boot/pane.rs` (Pane: region lifecycle, scroll/restore, println) vs `boot/pane/rows.rs` (TaskRows/selector/reserve_below) vs `boot/pane/tests.rs`; each a cohesive struct with methods, no free-function galaxy, no new traits.

# Acceptance
- `wc -l crates/slipfork/src/boot.rs crates/slipfork/src/boot/*.rs crates/slipfork/src/boot/**/*.rs` all <= 500.
- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p slipfork --lib --bins` green; golden byte test unchanged; lead re-runs `critical_status_live` on the VM (6/6).
