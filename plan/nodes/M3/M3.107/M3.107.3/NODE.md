---
id: M3.107.3
title: "Live build rendering in up: spinner phase rows, gutter-streamed output, collapse, one verdict"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.107.2]
paths:
  - "crates/slipfork-image/src/**"
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork/src/world/runtime/**"
  - crates/slipfork/src/cli/dispatch.rs
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 3
max_attempts: 3
escalation: expert
created: "2026-11-24T12:12"
updated: "2026-11-24T13:57"
---

# Goal
`slipfork up` renders the daemon's live build/step events as the narrative in the parent `# Visual contract`: phase rows with spinner+elapsed, output streamed under the running step, settled steps collapse to one `✓` row, failing step stays expanded with one verdict + `next:`.

# Context
- `boot.rs:197` drops `"log"` frames (`"log" => {}`); `boot.rs` `observe` dispatches frames; `Pane` (`boot/pane.rs`, 679 LOC, DECSTBM region) and `AsciiLine` (`boot/ascii.rs`) are the drivers; `Ticker` redraws.
- `boot/render.rs` (819 LOC, over budget): failure helpers `failed_row:264`, `failure_details:297`, `failure_block:397`, `failure_sentence:430` → replace with the v2 envelope in new `boot/failure.rs`; delete `✗ Failed to …` sentence path and its tests atomically. Live build region code goes in a new `boot/build.rs` (≤500 LOC), not into pane.rs.
- Event shapes from M3.107.2 (`step`, `log`, `failed` fields); verdict/stream-label helpers from M3.107.1 (`ui/palette.rs`).

# Acceptance
- PTY (real `script`/pty capture of `slipfork up`, multi-step Dockerfile that sleeps and writes both streams, last RUN exits 3): intermediate frames show the spinner row with ticking elapsed and live `│`/`┃` gutter lines inside the region; final scrollback = collapsed `✓ [i/n] …  Ns` rows, the failing step expanded with all its output in the gutter shape, `✗ RUN api failed (exit 3)`, one `  next: slipfork logs -r <id>`; no `crun`, no `Failed to`, prompt row intact after exit (region restored).
- Success build ends `✓ image api (N steps, Xs)`.
- `NO_COLOR` and non-TTY: append-only `→`/output/`✓` lines, zero `\x1b[`, no spinner frames. `--json`: frames pass through, zero escapes.
- `-v` adds the dim `  runtime: crun exited with code 3` row before `next:`.
- render.rs and pane.rs shrink or stay; new files ≤500 LOC. `cargo test -p slipfork --bin slipfork -- boot` + `--test critical_status_live` green.

# Styling
Parent `plan/nodes/M3/M3.107/NODE.md` `# Visual contract` (v2 + live build addendum) is authoritative.
