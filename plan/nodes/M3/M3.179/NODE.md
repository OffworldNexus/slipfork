---
id: M3.179
title: "M3.148c: narrow Palette/paint/table to pub(super)"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.178]
paths:
  - "crates/slipfork/src/ui/**"
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/status/**"
  - "crates/slipfork/src/doctor/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T18:26"
updated: "2026-11-25T18:58"
---

# Goal

Narrow `Palette`/`paint`/`table` to `pub(super)` and update the remaining call sites (`boot.rs:123`, `boot/render.rs`, `boot/status.rs:26`, `daemon/client.rs:66`, `commands/runs.rs:69`, `commands/docker/registry.rs`, boot test modules) so the palette has exactly one doorway.

# Acceptance

- No public `Palette`/`paint`/`table` outside `ui::report`; `rg 'pub fn .*Palette|pub use .*palette'` shows only the intended door.
- All boot/PTY output byte-identical; `make check`; Triad (arch); commit `[M3.179] ...`, done, no push.
