---
id: M3.2.8
title: "Rolling single-use warm Btrfs world pool (refill, timeout drain, slot-stable coverage/output mounts)"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.2.7]
paths:
  - "crates/slipfork-test/src/pool.rs"
  - "crates/slipfork-test/src/error.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_pool.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T17:25"
---

# Goal

Implement the rolling **single-use** warm Btrfs world pool: maintain `W` pre-booted pristine `test-<slot>` worlds, lease one per slot, refill in background threads, drain and destroy stuck/timed-out worlds, and mount slot-stable `coverage`/`output` directories at pre-warm boot.

# Context

- `ADR-0006` §4 condition 1 and `M3.2.1`'s blueprint fix the semantics; `BtrfsEngine` makes a slot a writable subvolume clone (~`11–24 ms`), so a pool of `W` worlds is cheap and a single-use slot guarantees hermetic isolation.
- Coverage env must be applied at boot, not by replaying `spawn_service` (`+0 ms` adoption rule).

# Design / Invariants

1. `src/pool.rs`: `WorldPool` over an injected `TestWorldFactory` (the preview from `M3.2.1`), configured with `W`, slot prefix `test-<n>`, and per-slot `.slipfork/runs/<run_id>/test-<n>/{coverage,output}` paths created **before** boot and passed into `WorldSpec.mounts` + `CoverageDriver::world_env`.
2. Leasing is single-use: a leased slot is removed from the free set, never returned; `release()` (success or failure) destroys the world and frees the slot number for refill. No world is reused across test batches.
3. Refill threads boot replacements up to `W` (bounded concurrency), with typed boot failures surfaced as `TestError::SlotBoot { slot, source }` rather than a poisoned pool. Timeout: a slot that does not reach readiness within the bounded budget is destroyed via the engine and replaced; the timeout is counted and reported.
4. Shutdown drains in-flight boots, destroys the remaining worlds, and leaves `.slipfork/runs/<run_id>/<slot>/{output,coverage}` on disk (output preservation is part of the contract).
5. No global state, no `static` pool, no thread sleeping on a bare `loop` without a bound; all threads owned by the pool and joined on shutdown.
6. Tests: `tests/critical_pool.rs` with a fake world factory asserts `W` pre-warm, single-use lease (never the same slot twice), refill after release, timeout drain+destroy, output-dir preservation, and clean shutdown with zero live worlds.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'single-use' crates/slipfork-test/src/pool.rs
grep -n 'test-' crates/slipfork-test/src/pool.rs
grep -n 'SlotBoot' crates/slipfork-test/src/pool.rs crates/slipfork-test/src/error.rs
grep -n 'output' crates/slipfork-test/src/pool.rs
! grep -rn 'static \|lazy_static\|OnceLock' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; the pool documents single-use slots and the `test-<n>` naming, types its boot error, and names the preserved output dir; the inverted `grep` prints nothing (no globals); `make rs-test` green.
