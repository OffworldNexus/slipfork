# Log M3.44.4

## 2026-11-21T20:17 [planner]

created (task) Reactive tokio boot DAG, per-node elapsed, world_epoch stamp & zero-rebuild <500ms fork

## 2026-11-21T21:12 [lead]

edited fields

## 2026-11-21T21:15 [lead]

status ready -> active

## 2026-11-21T21:15 [lead]

attempt 1/3 started

## 2026-11-21T21:15 [lead]

base dafad13f57dd190f6e5d171335d7b2225e5cc43b

## 2026-11-21T21:20 [lead]

edited fields

## 2026-11-21T21:25 [lead]

status active -> review

## 2026-11-21T21:25 [lead]

attempt 2/3 started

## 2026-11-21T21:25 [lead]

attempt 2: full-stack fork fixture (PG/Redis/S3 + seeded data), pre-fork CHECKPOINT, runtime.rs 501 split

## 2026-11-21T21:33 [lead]

edited fields

## 2026-11-21T21:33 [lead]

attempt 3/5 started

## 2026-11-21T21:33 [lead]

attempt 3: node_start after deps, multi-PG epoch, shard locks, NodeTask, dead waves, layout kinds, failed frame/state, ready remedy, fork timers, real Instant fork assert, per-node event assert, barrier regression, stamp gated

## 2026-11-21T21:38 [lead]

attempt 4/5 started

## 2026-11-21T21:38 [lead]

attempt 4: adapt supervise.rs to BootEvent name/state and scheduler API; finish gates

## 2026-11-21T21:43 [lead]

attempt 5/5 started

## 2026-11-21T21:43 [lead]

attempt 5 (final): multi-PG epoch, layout kind validation, real Instant fork assert, per-node event assert, WAL copy, bare names, ready remedy+progress; defer per-collaborator lock sharding to M3.45 perf gate (fork skips builds, no correctness impact)

## 2026-11-21T21:48 [lead]

accepted; deferred to M3.45: per-collaborator lock sharding in world/supervise.rs (perf-only, fork skips builds)

## 2026-11-21T21:48 [lead]

status review -> done

## 2026-11-21T21:48 [lead]

edited fields
