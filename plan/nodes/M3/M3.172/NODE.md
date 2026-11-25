---
id: M3.172
title: "Slice 2b: typed daemon boot facts (StepSettlement/FailureFacts/ConnectNotice), supervise without Palette"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/world/runtime/frames.rs
  - "crates/slipfork/src/daemon/lifecycle/up/**"
  - crates/slipfork/src/ui/report/row.rs
  - "crates/slipfork/src/ui/report/row/**"
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/commands/env.rs
  - crates/slipfork/src/commands/secret.rs
  - crates/slipfork/src/commands/docker/build.rs
  - crates/slipfork/tests/daemon_skew_live.rs
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-25T17:06"
updated: "2026-11-25T17:21"
---

# Goal (Report layer slice 2b)

Every boot frame carries typed facts instead of strings/`Palette`-rendered text:

- `StepSettlement` and `FailureFacts` (executed count, failed index, run id, next) produced in `daemon/reconcile/**` and emitted on every `started`/`finished`/failure frame.
- `ConnectNotice` via `connect(host, emit: &mut dyn FnMut(..))` so the ephemeral hint stays byte-identical (`tests/daemon_skew_live.rs:519`).
- `world/supervise.rs` and the up/fork lifecycles emit typed frames and zero `Palette`; delete `world::fail` and any remaining dual failure formatting.
- `commands/{env,secret,docker/build}` use the typed outcomes for their own failures.

Boot rendering is untouched, so all existing boot/renderer tests must stay green — this slice changes producers only.

# Acceptance

- `rg 'Palette' crates/slipfork/src/world` empty (or only a documented exception); no `error:`/`next:` strings produced outside `ui::failure`/`Report`.
- Byte-identical boot output for cached/fresh/failing/streaming PTY captures at HEAD vs this slice.
- `make check`, Triad (arch: one failure owner; tests: no vacuous frame assertions); commit `[M3.172] ...`, done, no push.
