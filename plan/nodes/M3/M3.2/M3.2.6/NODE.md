---
id: M3.2.6
title: "Typed CoverageDriver: coverage env/paths, report combination, coverage_fail_under"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.2.5]
paths:
  - "crates/slipfork-test/src/coverage.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_coverage.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Implement the built-in typed `CoverageDriver`: turn `[test]` (`coverage`, `coverage_report`, `coverage_fail_under`) into adapter-owned in-world env/paths, then combine per-slot shards into `html`/`xml`/`json`/`lcov` reports and enforce the threshold — with zero shell interpolation.

# Context

- `TestSpec`/`CoverageConfig`/`CoverageThreshold` already ship in `crates/slipfork-dag` (`ADR-0006` §5 is normative). `M3.2.1` locked `CoverageDriver { world_env, collect }`; `M3.2.3` declared it.
- Coverage must cost `0 ms` on slot adoption: env and mounts are applied at pre-warm boot, never by replaying `spawn_service`.

# Design / Invariants

1. `src/coverage.rs`: `CoverageDriver` implementations for the typed report formats with `world_env(world_coverage_dir) -> BTreeMap<String, String>` (Python `COVERAGE_FILE`, Node `NODE_V8_COVERAGE`, Rust `LLVM_PROFILE_FILE`, PHP harness path) and `collect(shard_dirs, formats, report_dir) -> CoverageSummary`.
2. `collect` runs typed `argv` (never `sh -c`), reads each shard's machine-readable output, merges percentages deterministically (line coverage, weighted by executed/total lines), writes the requested formats under `<run>/.../coverage`, and returns `CoverageSummary { percent, per_format_paths, fail_under }`.
3. `coverage_fail_under` is enforced uniformly for every adapter: below threshold returns `TestError::CoverageBelowThreshold { percent, threshold }`; `[test].coverage` with an adapter whose `coverage()` is `None` returns `CoverageUnsupported` **before** any world boots.
4. Missing shard/unparseable report for a requested format is a hard error naming the slot and format — never silently reported as 0% or 100%.
5. Tests: `tests/critical_coverage.rs` with fixture shard files asserts merge math, each format's output path, threshold pass/fail, unsupported-adapter rejection, and that `world_env` keys/paths match the documented adapter contract.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'CoverageBelowThreshold\|CoverageUnsupported' crates/slipfork-test/src/coverage.rs crates/slipfork-test/src/error.rs
grep -n 'COVERAGE_FILE\|NODE_V8_COVERAGE\|LLVM_PROFILE_FILE' crates/slipfork-test/src/coverage.rs
! grep -rn 'sh -c\|Command::new("sh")\|Command::new("bash")' crates/slipfork-test/src/coverage.rs
make rs-test
```

Expected: tests and clippy exit 0; both coverage errors exist; the three adapter env keys are present; the inverted `grep` prints nothing; `make rs-test` green.
