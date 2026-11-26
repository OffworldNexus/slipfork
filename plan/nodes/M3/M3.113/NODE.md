---
id: M3.113
title: "Build step rows render in instruction order, once, end row last"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/boot/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T16:14"
updated: "2026-11-24T16:21"
---

# Problem

Owner's live `up` (fast cached build) rendered:

```
✓ [2/6] COPY api  --from=ghcr.io/astral-sh/uv:latest  <1ms>
✓ [1/6] FROM api  python:3.14-trixie AS base  <1ms
✓ [2/6] COPY api  --from=… /uv /uvx /bin/  26ms
…
✓ image api (6 steps, 2.9s)
✓ [6/6] COPY api  . .  52ms
```

Rows print in **completion order**, a start row and its finish row both appear (duplicate index), and the build end row prints before the last step.

# Change

- The live renderer buffers finished step rows and emits them in **instruction index order**; a step's start row is replaced by its finish row (one row per index, ever).
- The image end row (`✓ image … (N steps, Xs)`) prints only after the last instruction's finish row.
- Cached and out-of-order concurrent completions follow the same rule; the bottom live area may still show running rows in start order.
- `NO_COLOR`/plain output is unaffected (already append-only), but assert there too.

# Acceptance

- Unit test feeding out-of-order `started/finished` events (COPY finishes before FROM) yields rows 1..N in order, one per index, end row last; same under `NO_COLOR`.
- Live `up` on the owner's Dockerfile shows 1..6 in order.
- `make check`; commit `[M3.113] ...`, done, no push.
