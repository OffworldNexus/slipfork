---
id: M3.2.7
title: "Artifact/fixture materializer: [artifacts.*], [fixtures.*], per_world post-origin, never in DigestCache"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.2.6]
paths:
  - "crates/slipfork-test/src/materialize.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_materialize.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Implement the fixture/artifact materializer: resolve `[artifacts.*]` through `CowArtifactStore` + `VolumeCloner`, materialise `[fixtures.*]` lineages (origin-independent ones sealed into `DigestCache` via `freeze_world`; `per_world = true` steps executed **post-origin** in the leased world and never cached).

# Context

- `crates/slipfork-dag` owns `DagPlanner`, `FixtureSpec.per_world`, `DigestCache`, `ArtifactStore`, `VolumeCloner`; `ADR-0003` §1 forbids `per_world` fixtures from ever entering `DigestCache` or being reported as `CacheAction::Cached`.
- `ResourcePlan::resolve(ResourceMode::Test)` already yields hermetic `initdb -> migrate` defaults; the materializer consumes that plan instead of re-deciding resources.

# Design / Invariants

1. `src/materialize.rs`: a typed `MaterializePlan` built from `DagPlanner` for one run (`ResourceMode::Test`), with ordered stages: artifacts → origin-independent fixture lineage (seal RO subvolume snapshot + `DigestCache` insert) → world clone → post-origin `per_world` steps (in the leased world, after `WorldOrigin` assignment) → readiness probe.
2. `per_world` steps are asserted by test to (a) never be inserted into `DigestCache`, (b) never be read from it, and (c) fail closed if a cache lookup would serve them (`DagError::OriginIndependentChildOfPerWorld` remains the parse-time guard).
3. Failures are typed and attributable: a failing fixture command returns `TestError::Fixture { id, stage, exit_code, tail }` and leaves the run tree intact for inspection.
4. All command execution goes through the injected world-exec capability with typed argv; no `shell=True` equivalent, no `cwd` guessing (`std::env::current_dir` is banned).
5. Tests: `tests/critical_materialize.rs` drives the real `DagPlanner` over a temp manifest (artifact + shared + `per_world` fixture chain) with a fake store/cloner and a fake world, asserting order, cache contents (or absence), stage failures, and `per_world` post-origin execution.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'per_world' crates/slipfork-test/src/materialize.rs
grep -n 'DigestCache' crates/slipfork-test/src/materialize.rs
grep -n 'ResourceMode::Test' crates/slipfork-test/src/materialize.rs
! grep -rn 'std::env::current_dir' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; `per_world`, `DigestCache`, and `ResourceMode::Test` are named; the inverted `grep` prints nothing; `make rs-test` green.
