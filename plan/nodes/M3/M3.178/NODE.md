---
id: M3.178
title: "M3.148b: runs/streams on ReportRenderer; delete LocalClock and ui/failure.rs"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.177]
paths:
  - crates/slipfork/src/commands/runs.rs
  - crates/slipfork/src/commands/streams.rs
  - "crates/slipfork/src/commands/streams/**"
  - crates/slipfork/src/commands/timeparse.rs
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/boot.rs
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/daemon/reconcile/progress.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/ui/failure.rs
  - crates/slipfork/src/ui/mod.rs
  - "crates/slipfork/src/ui/report/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T18:26"
updated: "2026-11-25T18:51"
---

# Goal

`commands/runs.rs`/`streams.rs` on `ReportRenderer`; delete `LocalClock`/`Session::clock` and `ui/failure.rs` (moving `split_remedy` to its single owner), updating `main.rs`, `world/supervise.rs`, `daemon/reconcile/progress.rs` and `daemon/client.rs`.

# Acceptance

- `logs`/`logs -r`/`-f`/`--last` and `runs` output unchanged (byte-exact, styled/`NO_COLOR`/`--json`); one failure surface only.
- `rg 'LocalClock|ui::failure|Session::fail' crates/slipfork/src` empty (or documented exceptions).
- `make check`, Triad (arch + tests), commit `[M3.178] ...`, done, no push.
