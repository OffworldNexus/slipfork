---
id: M3.30.9
title: "Journey restart assertions match the real multi-component restart output"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.30.4]
paths: [crates/slipfork/tests/critical_world_journey.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T04:57"
updated: "2026-11-19T05:14"
---

# Goal
`critical_world_journey` must assert the real restart output of a multi-component world with backing services.

# Evidence (VM test6, world_lifecycle_journey line 564)
The consolidated journey carried the old single-component literal block asserts. A restart of JOURNEY_SNOW actually prints:
```
[ready]     api (internal :8000 in world netns)
[ready]     idle (internal :8001 in world netns)
[restarted] api, idle, pg_clusters.main, redis_clusters.cache
world     acme-app (running, changed)
app       http://acme-app.localhost:46137
worktree  ...
next      ...
lineage   ...
```
so `block_lines[0] == "[restarted] api"` is wrong twice: the `[ready]` lines lead, and the restart restarts every component + backing service.

# Fix
In `world_lifecycle_journey_boots_edits_forks_and_reaps`, replace fixed-index assertions with content assertions that keep the same guarantees: a `[restarted] ` line exists and lists `api` (the edited component) and the backing services; a `[ready] ` line exists per component; the world line is `acme-app (running, changed)`; the app line starts with `app       http://acme-app.localhost`; `worktree`, `next      slipfork open |`, and `lineage   rootfs` rows exist; no `\x1b[` in the piped block; `[restarted]`/`[no change]` mutual exclusivity.
Do not weaken: keep asserting the restarted set equals what the daemon observed (compare to `ps --json` service ids if that is simpler).

# Acceptance
`cargo test --workspace --tests --no-run`, fmt, clippy; VM `critical_world_journey` exits 0.
