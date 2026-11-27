---
id: M3.175
title: "Slice 2c/S2: typed failure frames and one Failure projection for the boot pane"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.174]
paths:
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/ui/failure.rs
  - crates/slipfork/src/boot.rs
  - crates/slipfork/src/boot/failure.rs
  - crates/slipfork/src/boot/status.rs
  - "crates/slipfork/src/boot/status/**"
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/world/runtime/frames.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T17:25"
updated: "2026-11-25T17:46"
---

# Goal

The supervisor's real `BootEvent::Failed` frames carry no `failure`/`runtime`/`target` today; only the outer catch computes `failure_facts`. Add them, project `FailureFacts` into boot lines once (`ui/report/failure.rs`), delete `boot/failure.rs`, collapse `lifecycle_failure` to a single `Failure`, and fix the stale `world::fail` mention in `ui/failure.rs:4`.

# Acceptance

- A failing service and a failing build both render their `✗` row and one `next:` from the same `Failure`/`FailureFacts` value; no second failure formatter.
- PTY transcripts for failing flows unchanged; success flows unchanged.
- `make check`, Triad (arch: one failure owner; tests); commit `[M3.175] ...`, done, no push.
