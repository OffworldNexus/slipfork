---
id: M3.44.3
title: "Golden postgres-18 template + <1ms PGDATA CoW clone, Redis AOF clone, 2ms readiness polls"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.44.2]
paths:
  - "crates/slipfork-world/src/engine.rs"
  - "crates/slipfork-world/src/engine/**"
  - "crates/slipfork-world/src/resources.rs"
  - "crates/slipfork-world/src/boot/**"
  - "crates/slipfork-world/tests/critical_pod_runtime.rs"
  - "crates/slipfork-world/tests/critical_btrfs_live.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T20:17"
---

# Goal
Speed-of-light dev databases: create a golden readonly `initdb` template subvolume once, clone `PGDATA` by Btrfs snapshot in `< 1ms` with stale lock removal and the exact dev flag set, and run Redis with `--save "" --appendonly yes --appendfsync no`; both readiness-checked by `2ms` unix-socket polls instead of sleeps.

# Context
- `crates/slipfork-world/src/resources.rs:91-140` builds the postgres/redis pods with first-boot `initdb` and fixed sleeps; replace with: ensure `<btrfs_mount>/.slipfork-store/templates/postgres-18` (initdb once, then `BTRFS_SUBVOL_RDONLY` through existing `crates/slipfork-world/src/btrfs/ioctls.rs`), and per world `subvol snapshot` into `<world>/pg` (assert `< 1ms` with `Instant`), removing stale `postmaster.pid` and `pg_stat`/`postmaster.opts` remnants before start.
- Exact postgres dev flags (no TCP): `-c fsync=off -c synchronous_commit=off -c full_page_writes=off -c wal_level=minimal -c max_wal_senders=0 -c autovacuum=off -c shared_buffers=32MB -c dynamic_shared_memory_type=posix -c listen_addresses=''`, unix socket in `<world>/pg/.sock`; readiness = `2ms` interval `pg_isready -h <sock>` poll with ~2s budget, fail-closed with one `next:` line.
- Redis: CoW-clone the AOF dir per world, start `redis-server <conf> --save "" --appendonly yes --appendfsync no`, unix socket only, readiness = `2ms` `PING` poll; no `sleep 1`, no `/tmp` fallback.
- `crates/slipfork-world/src/engine.rs` is 416 LOC: move spawn/readiness into `crates/slipfork-world/src/engine/readiness.rs` (`wait_for_socket(path, budget, interval)` + `2ms` const) and `crates/slipfork-world/src/engine/pods.rs` (`PostgresSpec`/`RedisSpec` builders), leaving `engine.rs` <= 350 and each new file <= 250.
- Template lifecycle is idempotent and store-global (guarded by template dir + version file), invoked from `crates/slipfork-world/src/boot/**`; a world never writes the template. Missing `initdb`/`redis-server` or non-Btrfs store → single-line `next:` remedy, zero degraded fallback.
- Do not change DAG scheduling semantics (`M3.44.4`) or the S3 server (`M3.44.2`); readiness results are reported as ordinary pod events. Zero mocks — real Btrfs + real `postgres`/`redis-server`.

# Acceptance
- `cargo test -p slipfork-world --test critical_pod_runtime` green: template subvolume is `RDONLY`, PGDATA snapshot `< 1ms` (asserted `Instant`), stale `postmaster.pid` removed, running `postgres` argv equals the exact flag set, `2ms` poll (no fixed sleep), redis `--save ""` argv parity, AOF isolation between two worlds.
- `cargo test -p slipfork-world --test critical_btrfs_live` and `cargo test -p slipfork-world` (`world_plan_features`) green.
- `wc -l crates/slipfork-world/src/engine.rs` <= 350 and no file under `resources/` or `engine/` exceeds 500 LOC.
- `cargo clippy -p slipfork-world --all-targets -- -D warnings` and `uv run --no-project tools/guard.py` exit 0.
