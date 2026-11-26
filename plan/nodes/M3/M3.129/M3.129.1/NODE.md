---
id: M3.129.1
title: "critical_status_live multi-step build fixtures need WORKDIR after M3.118"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: []
paths: [crates/slipfork/tests/critical_status_live.rs]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T21:10"
updated: "2026-11-24T21:11"
---

# Problem
After M3.118 (`2727e8d`) the dev-image planner fails closed when a Dockerfile copies the build context without a `WORKDIR` ("dev image `web` … has no WORKDIR (runs from `/`)"). `critical_status_live::live_boots_once_paints_the_panel_reconciles_orphans_and_replays_logs` still writes `FROM python:3.13-slim\nCOPY . /app\nRUN …` (sections 6, 7, 9 around `critical_status_live.rs:1182`, `:1418`, `:1527`), so `up --json` fails before any `step` frame and the `FROM`-row assertion panics at `:1255`. Found by the M3.111 VM gate; pre-existing, unrelated to M3.111 (its diff to this file is golden-text only: lowercase inner keywords).

# Fix
Add `WORKDIR /app` after `COPY . /app` in those three fixtures and renumber every dependent expectation (`1/4 FROM` → `1/5 from`, `2/4 COPY` → `2/5 copy`, `3/4 RUN` → `4/5 run`, `4/4 RUN` → `5/5 run`, bar label `step 3/4 RUN` → `step 4/5 RUN`, `source Dockerfile:4` → `Dockerfile:5`, failure-frame checks). Keep the two-level, lowercase, indent-4 gutter, single verdict + remedy assertions intact.

# Acceptance
- `SLIPFORK_TEST_ROOT=/mnt/slipfork-live cargo test -p slipfork --features live --test critical_status_live -- --test-threads=1` green on the Btrfs VM.
- `make check`; commit `[M3.129] …`.,
