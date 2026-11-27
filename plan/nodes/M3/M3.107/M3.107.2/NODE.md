---
id: M3.107.2
title: "Live build events: per-instruction step/log frames + failure facts from the daemon"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.107.1]
paths:
  - "crates/slipfork-image/src/**"
  - "crates/slipfork-image/tests/**"
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/world/runtime.rs
  - "crates/slipfork/src/world/runtime/**"
  - crates/slipfork/tests/critical_status_live.rs
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-24T12:04"
updated: "2026-11-24T12:45"
---

# Goal
The daemon tells the client, as it happens, every Dockerfile instruction of a build (start, live output, settle), and every boot step's command; failures carry the facts the envelope needs. No client rendering here (M3.107.3).

# Context
- Log frames come from `world/supervise.rs:532` `EventLog::emit` (`BootSink`/`EventLog`); `world/runtime.rs:248` opens the step capture.
- `slipfork-image/src/runtime.rs:175` `Error::StepFailed { line, instruction, message: "`crun` exited with code N", next }` — make `exit_code: Option<i32>` a field; crun text becomes a `-v`-only runtime detail, not the Display headline.
- `world/runtime/images/log.rs` `BuildLog` is the image-build sink (pure after M3.107.1).
- Instruction loop lives in `slipfork-image/src/builder/**` / `steps.rs`; base pull in `puller.rs`/`fetch.rs`; context walk in `context.rs`.

# Acceptance
- `--json` `up` on a multi-step Dockerfile emits, in order and live (timestamps prove interleaving): per instruction `{"event":"step","image","index","total","instruction":"RUN","args","state":"started"}` BEFORE it executes, its `{"event":"log","image","index","stream","line","ts_ms"}` lines, then `state:"done"|"failed"` with `elapsed_ms` (+`exit_code` on RUN failure). FROM/pull, COPY and context walk emit `started` immediately (no silent freeze).
- `failed` boot frame carries `phase`, `instruction`, `source` (`Dockerfile:N`), `exit_code`, `run_id`, `next`; `crun exited …` only in a `runtime` field shown under `-v`.
- Service/hook steps: `started` frame carries `command`; boot-phase output emitted as `log` frames with `stream`.
- Lab (real crun, no mocks): Dockerfile `FROM <cached base>` / `COPY` / `RUN sh -c 'echo a; sleep 1; echo b >&2'` / `RUN sh -c 'echo out-line; echo err-line >&2; exit 3'`; assert event order, streams, `exit_code:3`, zero `\x1b[`.
- Files <= 500 LOC (do not grow files already over); `cargo test -p slipfork-image -p slipfork --test critical_status_live` green.

# Styling
Parent `plan/nodes/M3/M3.107/NODE.md` `# Visual contract` (v2 + live build addendum) is authoritative.
