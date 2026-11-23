---
id: M3.50.3
title: "8-component <1.0s fork: no CHECKPOINT, conditional edges, image reuse"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.50.1]
paths:
  - "crates/slipfork-world/src/resources/**"
  - "crates/slipfork-world/src/boot/**"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/tests/boot_dag.rs"
  - "crates/slipfork-world/tests/critical_pod_runtime.rs"
  - "crates/slipfork-image/src/**"
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/fork_nonbtrfs.rs
  - "support/**"
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T13:39"
---

# Goal
`slipfork fork` on the full 8-component `~/demo` (4 resources + 4 services) completes in `< 1.0s` CLI wall-clock, reuses parent `image` subvolumes as `cached`, starts services in parallel with `pg`/`redis` when stamps are current, and performs zero pre-fork SQL `CHECKPOINT`; `support/demo` is restored and `critical_fork_live` boots all 8 rows.

# Context
- Delete `crates/slipfork-world/src/resources/pg/checkpoint.rs` and its caller `crates/slipfork/src/daemon/lifecycle/fork.rs:389-401` (spec 02 §4: zero pre-fork `CHECKPOINT`).
- Direct exec is already asserted: `crates/slipfork-world/src/resources/pg/launcher.rs` (absolute `postgres`, `docker-entrypoint.sh` ban at `crates/slipfork-world/tests/pod_control_lab.rs:196-210` and `tests/critical_pod_runtime.rs:872`) and `crates/slipfork-world/src/resources/cache.rs` (direct `redis-server`). Keep the `2ms` readiness polls of spec 03 §3.
- The blocking edge: `crates/slipfork-world/src/boot/graph.rs:93-99` chains **every** resource step into every service node. Make it conditional per spec 03 §3 rule 2: on current `world_epoch` + matching input digests, `setup`/`before-start`/`pre-deploy` verify stamps immediately after their `image` and service nodes carry only image + hook edges; resources remain readiness barriers for `:80` swap, `✓ Ready`, and `post-deploy`. Rule 1 (stale/unstamped: `pre-deploy` awaits all resources) must stay intact.
- Scheduling/waits: `crates/slipfork-world/src/boot/schedule.rs:23-48` (`BootState::Cached/Pulled/Built` + `as_str`), `crates/slipfork-world/src/boot/stamp*`, `crates/slipfork-world/src/boot/stamp/epoch.rs`.
- Parent subvolume reuse: `crates/slipfork/src/world/runtime/images.rs` — on `fork` with matching `world_epoch`, adopt the parent world's resolved read-only `image` subvolumes from `<world>/.slipfork-world-stamp.json` (`cached`, ~1ms) with no re-seal. Cache hits must report `cached`, never `pulled`/`built`; failed builds must append their log to `<store>/logs/<world>/<id>.log` via the writer from M3.50.1.
- `--no-sync`: service argv uses `.venv/bin/<prog>` or `uv run --no-sync` (`UV_NO_SYNC=1`) so `uvicorn`/`worker` never re-run `uv` lock checks (Forkfile side: `support/demo/Forkfile`).
- Fixture: `crates/slipfork/tests/critical_fork_live.rs:40-58` boots only pg, cache, 1 S3, 1 image, api. Restore `support/demo` (deleted by M3.37 — `git show 208e237 -- support/demo`): `Forkfile`, `Dockerfile`, `.dockerignore`, `.gitignore`, `README.md`, `app/{api,clients,seed,worker}.py`, `front/{Dockerfile,server.py}` with 8 rows `pg`, `cache`, `uploads`, `assets`, `api`, `worker`, `front`, `mailpit` and `arg VITE_CDN_URL = static-cdn.url`, whose digest must stay stable across forks.
- Budget: spec 02 §4 and spec 03 §3 (`~0.5-0.8s` target, hard ceiling `< 1.0s`).

# Acceptance
- `cargo test -p slipfork-world --test boot_dag`: current-epoch graph drops resource->service edges; stale epoch keeps `pre-deploy` gated on `pg`/`redis`/`s3`.
- `cargo test -p slipfork-world --test critical_pod_runtime` (absolute argv[0]) green.
- `cargo test -p slipfork --test critical_fork_live` on real Btrfs: 8 rows ready, 1 discarded warm-up + 3 timed samples, `median < Duration::from_secs(1)`; `PG_VERSION`/`pg_wal` mtimes unchanged across snapshot prep; a service `Started` interval overlaps `pg` not yet `Ready`; dirty `drop` without `-f` refuses.
- `cargo test -p slipfork --test fork_nonbtrfs`, `cargo clippy --workspace --all-targets -- -D warnings`, `uv run --no-project tools/guard.py`.
