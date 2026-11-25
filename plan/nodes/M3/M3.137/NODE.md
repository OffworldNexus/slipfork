---
id: M3.137
title: "Slice 3d: reconcile ordering that keeps the old world serving, restart in place, full journeys"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.136]
paths: ["crates/slipfork/src/daemon/**", "crates/slipfork/tests/**", "specs/02-cli-ux.md"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T00:05"
updated: "2026-11-25T01:48"
---

# Goal

Reconcile ordering: build new heads while the old world keeps serving, then swap; `restart` restarts in place when heads are unchanged; a failed build leaves the old world untouched and reports one remedy. Land the full journeys (failed RUN, build→up cached, restart in place, old-world-keeps-serving).

# Acceptance

- Live VM: failed RUN during `up` does not stop a healthy world; `restart` unchanged restarts 2 pods in place; new image → swap then boot; `up` unchanged is idle with no image row.
- `make check`, full live journeys, Triad, commit `[M3.137] ...`, no push.
