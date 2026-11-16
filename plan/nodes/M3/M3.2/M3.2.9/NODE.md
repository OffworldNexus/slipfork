---
id: M3.2.9
title: "TestCoordinator: run tree, batch vs world scheduling, coverage enforcement, TestEvent stream"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.2.8]
paths:
  - "crates/slipfork-test/src/coordinator.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_coordinator.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Implement `TestCoordinator`: the typed orchestration loop that materialises fixtures/artifacts, leases warm slots, dispatches `batch` vs `world` items to adapters, applies coverage collection, enforces `coverage_fail_under`, and reports per-item results over the canonical `.slipfork/runs/<run_id>/<slot>/` tree.

# Context

- `M3.2.1` locked the seam (`DaemonControl::test` → `TestEvent`) and `M3.2.2`–`M3.2.8` supplied the pieces: protocol, sockets, commands, coverage, materializer, pool.
- This node is the composition point: it may add no new external dependency and no new engine capability; if something cannot be expressed through the locked traits, stop and report a `direction-challenge` instead of widening them.

# Design / Invariants

1. `src/coordinator.rs`: `TestCoordinator` owns run setup (`run_id` from injected entropy, run tree dirs), scheduling, failure aggregation, and a bounded `-j N` concurrency. It consumes `ResourcePlan`/`MaterializePlan`, `WorldPool`, `Vec<Arc<dyn TestAdapter>>`, and emits typed `TestEvent`s (`RunStarted`, `ItemResult`, `Coverage`, `RunFinished`) to an injected sink.
2. Two-tier scheduling exactly as `ADR-0006` §4.3: `isolation = batch` items share one leased world per fixture; `isolation = world` items each get a dedicated single-use slot; a failed `batch` item never aborts the remaining items of its world, but a failed world boot aborts that world's items only.
3. Failure semantics: exit code non-zero iff any item failed or coverage is below threshold; skipped items are reported and never counted as passed; the final event carries counts and the coverage summary.
4. Coverage: one `CoverageDriver::collect` per run over all slot coverage dirs; `coverage_fail_under` enforced once, after all items.
5. `.slipfork/runs/<run_id>/<slot>/output` survives teardown for every slot (asserted), and the run tree path is created with `0700`-equivalent confinement under the repo root — never `$HOME`, never `/tmp`.
6. Tests: `tests/critical_coordinator.rs` composes fake adapters + real `DagPlanner` + fake world factory to assert scheduling tiers, event stream, failure counts, coverage threshold failure, output preservation, and that `-j 1` and `-j 4` yield identical pass/fail outcomes.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'TestEvent' crates/slipfork-test/src/coordinator.rs
grep -n 'DedicatedWorld\|IsolationTier' crates/slipfork-test/src/coordinator.rs
grep -n 'coverage_fail_under\|fail_under' crates/slipfork-test/src/coordinator.rs
! grep -rn 'std::env::current_dir\|std::env::var' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; the coordinator emits typed events, honours both isolation tiers and the threshold; the inverted `grep` prints nothing; `make rs-test` green.
