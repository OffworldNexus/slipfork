---
id: M3.112
title: "Service start failures: capture output, fail fast on premature exit, record exit/reason"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109]
paths:
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/runtime/ready.rs
  - crates/slipfork/src/world/runtime/execute.rs
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T16:14"
updated: "2026-11-24T16:41"
---

# Problem

A service fails to start (`@uv run blastit dev --port 8000` exits); `slipfork up` prints `✗ start web failed` and `slipfork logs -r <id>` shows the header but **no output and no exit**. The child's stderr went only to the service pod log (`<service>.log`), never to the run capture, and the exit/reason was not recorded on the run.

# Change

- When a service start step runs, tee its stdout/stderr into the **run capture** (stream + timestamp), in addition to the pod log. If the service exits during the start window, record the exit code and a one-line reason on the run record.
- The verdict becomes `✗ start web failed (exit N)`; the capture shows the service's own `┃` lines. `slipfork logs web` still works.
- Keep retries/long-running output from unbounded growth (the normal pod log already handles retention).
- Do not change the build-step path.

# Acceptance

- Live: a service that writes a marker to stderr then exits 1; `up` fails; `logs -r` shows the marker with `┃` and `(exit 1)`; `logs web` shows it too. Clean up the world.
- `make check`, `critical_world_journey`; commit `[M3.112] ...`, done, no push.
