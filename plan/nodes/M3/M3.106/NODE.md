---
id: M3.106
title: "Structured run captures: stdout/stderr labels, timestamps and a step header"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.104]
paths:
  - "crates/slipfork-image/**"
  - "crates/slipfork/src/world/runtime/images/**"
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork-world/src/pod_log.rs"
  - "crates/slipfork-world/src/run_log.rs"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/src/bin/world_init/pod/launch.rs"
  - "crates/slipfork-world/src/bin/world_init/stream/tail.rs"
  - "crates/slipfork-world/tests/pod_control_lab.rs"
  - "crates/slipfork-world/tests/critical_pod_runtime.rs"
  - "crates/slipfork-world/tests/run_logs.rs"
  - "crates/slipfork-world/tests/critical_log_store.rs"
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/commands/timeparse.rs
  - crates/slipfork/src/boot/render.rs
  - "crates/slipfork-image/tests/**"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:45"
updated: "2026-11-24T11:58"
---

# Problem

`slipfork logs -r 8f656d` after a failed `RUN` printed only the installer's stderr line plus the failure summary: no stdout, no stream labels, no timestamps, no step header. In `crates/slipfork-image/src/runtime.rs:163-210`, `crun`'s stdout and stderr readers both push bare `String`s into one channel and the sink is `&mut dyn Write`, so the provenance and time are lost before the capture.

# Contract

- Image `RUN` (and hooks/setup that stream) carry a typed **stream** (`stdout`/`stderr`) and a **timestamp** into the capture, not merged text.
- `pod_log` entries gain a `stream` field (raw log line framing + index); `logs -r`/`logs <image>` render `HH:MM:SS.mmm  out|err  <line>` by default (dim gutter/label, no ANSI under `NO_COLOR`/`--json`), and `--json` carries `stream` + `ts_ms` + `line`.
- Replay starts with one **header row** from the run record: `#<run_id>  <kind>  <target>  <command>  <started_at>`, then the lines; the failure summary follows the capture, never inside it.
- Live `logs -f` keeps the merged view but labels each line's stream.
- `slipfork-image` defines the step sink (a small `StepLog`/`Stream` type owned by the image crate); the CLI `BuildLog` adapts it and calls `pod_log::append_stream`. No mock traits.

# Acceptance

- Real lab: a `RUN` that writes to **both** stdout and stderr, then exits 1; `logs -r <id>` shows both labeled lines with timestamps and the header; `--json` has both streams; the failure summary is outside the capture.
- `make check`, `cargo test -p slipfork-image`; commit `[M3.106] ...`, done, no push.
