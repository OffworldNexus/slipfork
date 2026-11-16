---
id: M3.2.10
title: "PytestAdapter (Rust): warm Zygote runner over RunnerProtocol v1 + Python coverage combine"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.2.9, M3.2.11]
paths:
  - "crates/slipfork-test/src/pytest.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_pytest.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T17:25"
---

# Goal

Implement `PytestAdapter` on the Rust side: launch the bundled `pytest-slipfork` runner against a leased world, negotiate `RunnerProtocol v1` over `runner.sock`, request the `<1 ms` warm Zygote `os.fork()` path, and collect Python coverage shards through the typed `CoverageDriver`.

# Context

- `pytest-slipfork` (**Adapter #1**, `ADR-0006` §2/§5) subtracts `pytest-django`'s `test_<db>`, `live_server`, and `serialized_rollback` because the world already owns the migrated, seeded, routed stack.
- The Python plugin (`M3.2.11`) is written in parallel with disjoint paths; this node owns only the Rust adapter and its tests, and must agree with `M3.2.3`'s frames exactly.

# Design / Invariants

1. `src/pytest.rs`: `PytestAdapter` implements `TestAdapter`: `discover` sends pytest filters (`-k`, node ids) verbatim; `run_items` sends `world_id`, `base_url` (`WorldOrigin` URL), `output_dir`, and items; it streams results through the callback and maps plugin-level failures to `TestError::Protocol` with the plugin's stderr tail.
2. The adapter never boots Python itself: it starts the runner inside the leased world through the injected world-exec capability, with env from `WorldSlotContext` (`BASE_URL`, `DATABASE_URL`, `SLIPFORK_OUTPUT_DIR`, `COVERAGE_FILE`/coverage dir), and a bounded startup budget.
3. Coverage: `coverage()` returns a `PytestCoverageDriver` implementing `CoverageDriver` with `COVERAGE_FILE=<slot>/coverage/.coverage.<shard>` and a typed `coverage combine` + `coverage {html,xml,json,lcov}` argv combination (no shell), merging shards from all slots.
4. Health: a plugin that exits before `run_complete`, returns an unknown `version`, or emits results for undiscovered ids is a hard error naming the slot, the last frame, and the plugin's exit status.
5. Tests: `tests/critical_pytest.rs` runs the *real* Python plugin from `packages/pytest-slipfork` over a real `AF_UNIX` socket in a temp world directory (skipping nothing: if `python` is unavailable the test fails with the exact interpreter requirement), asserting the conformance suite, a passing and a failing pytest item, and a coverage shard merge.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'assert_test_adapter_conformance' crates/slipfork-test/tests/critical_pytest.rs
grep -n 'BASE_URL' crates/slipfork-test/src/pytest.rs
grep -n 'COVERAGE_FILE' crates/slipfork-test/src/pytest.rs
! grep -rn 'sh -c\|Command::new("sh")\|Command::new("bash")' crates/slipfork-test/src/pytest.rs
make rs-test
```

Expected: tests and clippy exit 0 (the plugin is exercised for real over a socket); the conformance suite runs against `PytestAdapter`; `BASE_URL` and `COVERAGE_FILE` are adapter-owned; the inverted `grep` prints nothing; `make rs-test` green.
