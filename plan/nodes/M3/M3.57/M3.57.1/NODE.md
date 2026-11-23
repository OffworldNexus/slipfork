---
id: M3.57.1
title: "Parallelism: warm-up stamp reuse + concurrent pod teardown"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/src/bin/world_init/**"
  - "crates/slipfork-world/src/supervisor/**"
  - "crates/slipfork/src/daemon/lifecycle/**"
  - "crates/slipfork/src/world/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T22:37"
updated: "2026-11-22T22:54"
---

# Goal
Warm `up` reuses the trusted WorldStampStore (skip unchanged pre-deploy) and `down` stops all pods concurrently with exact -pgid reaping.

# Acceptance
- Warm down/up: pre-deploy `up to date`, services released at t=0.
- 6-pod concurrent stop with zero survivors; live-lab overlap test.
- `cargo test -p slipfork -p slipfork-world` green.
