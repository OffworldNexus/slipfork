---
id: M3.111
title: "Log polish: whole-build replay header and quiet Broken pipe"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.108]
paths:
  - "crates/slipfork/src/commands/**"
  - "crates/slipfork/src/daemon/lifecycle/logs/**"
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/world/runtime/images/**"
  - crates/slipfork/src/world/runtime/capture.rs
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork-world/src/pod_log.rs"
  - "crates/slipfork-world/src/run_log.rs"
  - "crates/slipfork-world/src/bin/world_init/stream/lines.rs"
  - "crates/slipfork/tests/**"
  - "specs/02-cli-ux.md"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T14:55"
updated: "2026-11-24T21:11"
---

# Two log defects found after the M3.107 gate

1. **Whole-build replay under a single-step header.** `slipfork logs -r <id>` for an image build prints every instruction's output, but the header names only the failing `RUN`. Replay should show a step row per instruction (keyword/target/command, timestamps under each), so the whole build reads as a sequence. Keep the one final verdict.
2. **Broken pipe noise.** `slipfork logs -r --json | head` prints `error: failed to write output: Broken pipe` twice. A closed stdout must end the stream quietly with the last successful exit code (or 0), no error lines, and no ANSI.

# Acceptance

- Live replay of a multi-step image build shows one row per instruction plus its gutter output, then the single verdict.
- `logs -r --json | head -1` exits quietly (no `error:`, no stderr spam), and the same for `logs --runs --json | head`.
- `make check`; commit `[M3.111] ...`, done, no push.
