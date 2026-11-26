---
id: M3.115
title: "up and restart converge when the Forkfile, Dockerfile or build context changes"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109, M3.112]
paths:
  - crates/slipfork/src/daemon/lifecycle/up.rs
  - crates/slipfork/src/daemon/lifecycle/core.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/src/daemon/boot.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/world/up.rs
  - "crates/slipfork-world/src/boot/mod.rs"
  - "crates/slipfork-world/src/boot/stamp/epoch.rs"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/tests/boot_dag.rs"
  - "crates/slipfork-image/**"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T16:19"
updated: "2026-11-24T19:03"
---

# Problem

`crates/slipfork/src/daemon/lifecycle/up.rs::fingerprint` hashes only the `Forkfile` source, `.slipfork/features.json`, the no-default flag and edits. Editing a `Dockerfile` (or any build-context file) leaves the fingerprint unchanged, so `up_plan` returns `Idle`/`Heal` and the world keeps the old image; the owner saw `service \`web\` is not running` after changing the Dockerfile, instead of a rebuild.

# Change

- For every `image` in the compiled Forkfile, include in the `up` fingerprint: the resolved Dockerfile path + content hash, and the **same build-context digest the image cache/state key uses** (call the canonical function; honour `.dockerignore`; do not re-implement the walk). Include `from` refs/args only if the image cache already keys on them (no network in the fingerprint).
- `inputs_predate` must also consider the Dockerfile mtimes so the "predates the boot" fast path is truthful.
- On a changed Dockerfile/context, return `Reconcile` (existing path); the builder rebuilds only the affected stages and the world reboots.
- No behaviour change when nothing changed: `up` stays `Idle`/cached.

# Acceptance

- Live: boot a world; edit a `RUN` line in its Dockerfile; `up` rebuilds the image (new run ids/real timings, not `cached`) and the new behaviour is live; a following unchanged `up` is cached/Idle.
- Unit test for the fingerprint (Forkfile only vs Dockerfile vs context file).
- `make check`, `critical_world_journey`; commit `[M3.115] ...`, done, no push.
