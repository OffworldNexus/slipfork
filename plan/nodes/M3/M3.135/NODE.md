---
id: M3.135
title: "Slice 3b: daemon image.build/image.pull RPCs with streamed frames; CLI is a thin client"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.120]
paths:
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/commands/docker/**"
  - "crates/slipfork-world/src/store/**"
  - "crates/slipfork-world/src/snapshot/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T00:05"
updated: "2026-11-25T00:05"
---

# Goal

Move image building/pulling behind the daemon so one process owns the image cache decision (specs/03 §6). Add `image.build`/`image.pull` RPCs that stream `plan`/`started`/`finished` frames, make `slipfork docker build`/`docker pull` thin clients of them, and have `docker pull` refresh the remembered digest in `images/refs`.

# Acceptance

- Live VM private tree: `docker build` warm 4 hits/0 misses, cold 0/4, frames identical to today's shape; `docker pull` refreshes `images/refs` and the next build uses the new digest; `--json`/`NO_COLOR` zero escapes.
- No second cache layer, no CLI-side build path left behind (grep gate).
- `make check`, Triad, commit `[M3.135] ...`, no push.
