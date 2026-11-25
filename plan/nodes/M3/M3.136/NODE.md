---
id: M3.136
title: "Slice 3c: daemon pre-plans world images into WorldHeads; supervisor consumes heads; delete ImageCache"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.135]
paths: ["crates/slipfork/src/world/**", "crates/slipfork/src/daemon/**", "crates/slipfork-world/src/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T00:05"
updated: "2026-11-25T00:58"
---

# Goal

The daemon pre-plans and seals every world image, writes `WorldHeads`, and `__supervise` boots only from those heads; delete `ImageCache` and `world/runtime/images/*`, moving build logs/run notes into `daemon/reconcile/{mod,progress}.rs`.

# Acceptance

- Live: `up` builds once, supervisor boots from the head; Dockerfile edit rebuilds; copied-file edit rebuilds; `.git`-only stays cached; dev `WORKDIR` live; `fork` cached; `--no-cache` cold.
- Grep gate: no `ImageCache`, no `world/runtime/images`. `make check`, Triad, commit `[M3.136] ...`, no push.
