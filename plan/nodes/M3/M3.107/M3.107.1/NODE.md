---
id: M3.107.1
title: "Capture truth: stream-tagged replay, pure captures, run record names the RUN that ran"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-image/src/**"
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/commands/runs.rs
  - crates/slipfork/src/commands/timeparse.rs
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/daemon/lifecycle/replay.rs
  - crates/slipfork/src/daemon/lifecycle/mod.rs
  - "crates/slipfork/src/world/runtime/**"
  - crates/slipfork/src/world/runtime.rs
  - "crates/slipfork/src/ui/**"
  - crates/slipfork/src/boot.rs
  - crates/slipfork/src/boot/render.rs
  - "crates/slipfork-world/src/boot/schedule.rs"
  - "crates/slipfork-world/src/pod_log.rs"
  - "crates/slipfork-world/src/run_log.rs"
  - "crates/slipfork-world/src/log_store.rs"
  - "crates/slipfork-world/tests/critical_log_store.rs"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-24T12:04"
updated: "2026-11-24T12:27"
---

# Goal
`slipfork logs -r <id>` replays exactly what the shell said, labelled by its real stream, under a header naming the instruction that actually ran.

# Context
- `daemon/lifecycle/logs.rs::read_captures` emits `line_event(pod, kind, text, ts)` with no stream -> renderer defaults `out`. `pod_log` already stores `O`/`E`.
- `world/runtime/images/log.rs::BuildLog::fail` (l.59) appends `build `api` failed: ...` via Stdout-hardcoded `emit` (l.42). Delete that path.
- `world/runtime.rs:248` passes `step.subject` (image id) as run `command` for images -> header `#id image api api`.
- `commands/streams.rs:433 print_header`, `:241 stream_label`, palette `ui/palette.rs`.
- `logs.rs` is 1034 LOC: do not grow it (net <= 0; move replay code to `daemon/lifecycle/replay.rs` if needed).

# Acceptance
- Capture of a `RUN` writing both streams: `E` lines replay as `err`, `O` as `out`; zero synthetic summary lines in `.log`.
- Image run record carries the phase + command of the last executed instruction (failing one on failure): `phase:"run"`, `instruction:"sh -c \"...\""`, `source:"Dockerfile:12"`. Header mirrors `up` (owner spec): `#<id>  RUN  <target>  <command>  <started>`, e.g. `#5be421  RUN  api  sh -c "curl … | sh && apt install blastit"  2026-11-24 12:00:03`; `command` omitted when == target; no image id twice. Lines below in the parent v3 gutter shape (no `out`/`err` words).
- Styles: per parent v3 (`#id` dim, kind dim, target+command bold, started dim, one blank line, `  HH:MM:SS.mmm │|┃ text`); `NO_COLOR`/`--json` zero `\x1b[`; `--json` keeps `stream`, `phase`, `instruction`, `source`, `exit_code`.
- `cargo test -p slipfork-world -p slipfork --test critical_world_journey` green.

# Styling
The parent `plan/nodes/M3/M3.107/NODE.md` `# Visual contract` (v2) is authoritative for every colour/glyph/indent; where this node says otherwise, the parent wins.
