---
id: M3.158
title: Root registration still reads a stale world record on first launch (residue from M3.140)
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.141]
paths:
  - "crates/slipfork/src/world/**"
  - crates/slipfork/tests/critical_world_journey.rs
  - "crates/slipfork/src/daemon/lifecycle/up/**"
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/daemon/host.rs
  - crates/slipfork/src/daemon/lifecycle/core.rs
  - crates/slipfork/src/daemon/image/build.rs
  - crates/slipfork/src/commands/open.rs
  - crates/slipfork/src/commands/features.rs
  - crates/slipfork/src/status/tree.rs
  - "crates/slipfork-world/src/layout/family.rs"
  - "crates/slipfork-world/src/lib.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T11:21"
updated: "2026-11-25T12:58"
---

# Problem

`M3.140` deleted the legacy world registries, but its report notes: `world_name()` still reads a stale `.slipfork/world.json` fallback on the **first** root registration (`src/world/**`), i.e. a world created by an older build can be adopted with the wrong identity before the ledger row exists. No active node owns that file.

# Fix

- Root registration reads identity from the ledger (and the project path), never from a legacy record; a stale `.slipfork/world.json` is ignored (and removed by the existing `open_writer` unlink path) rather than consulted.
- One owner for "what world is this": the ledger row, with the path as the key.
- Fail closed if a stale record disagrees with the ledger (one line + `next: slipfork up` to re-register) instead of silently preferring either.
- Delete the stale reader and any remaining `world_id_for_project`-style helper in the CLI crate (keep the `slipfork-world` layout API if the engine needs it).

# Acceptance

- Lab: a project with an old `.slipfork/world.json` naming a different world registers as the path's world, ignores/removes the record, and logs nothing scary.
- Live VM: `up` on such a project converges (no duplicate rows, no orphan world); a second `up` is idle.
- `make check`; Triad; commit `[M3.158] ...`, done, no push.
