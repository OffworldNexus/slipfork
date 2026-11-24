---
id: M3.121
title: "Build-context COPY edits do not rebuild; M3.115's live test fails at HEAD"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.118]
paths:
  - "crates/slipfork-image/src/context.rs"
  - "crates/slipfork-image/src/inputs.rs"
  - "crates/slipfork-image/src/builder/**"
  - crates/slipfork/src/world/runtime/images.rs
  - "crates/slipfork-image/tests/critical_dockerfile_live.rs"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T17:46"
updated: "2026-11-24T20:01"
---

# Problem (found by lead during M3.117 VM capture, binary at the M3.117 tree)
Repro on real Btrfs+crun: project with `image api . { dockerfile Dockerfile … }`, Dockerfile `FROM python:3.13-slim` / `RUN echo one` → `up` builds 2 steps. Edit to `RUN echo two; exit 9` → `up` (world running) prints nothing; `down` then `up` → `✓ [1/2] image api cached 11ms`. The changed (and failing) instruction never runs. Expected: the Dockerfile content (and every instruction's as-written text, args, and COPY context digest) is part of the image cache key; an edited instruction invalidates from that step on; a running world with a changed image rebuilds on `up`.

# Acceptance
- Live (critical_dockerfile_live, real crun): edit a RUN line → next `up` rebuilds from that step (earlier steps cached), runs the new command, and a failing edit fails with `✗ RUN … failed (exit 9)`.
- No regression in cached reuse when nothing changed.
