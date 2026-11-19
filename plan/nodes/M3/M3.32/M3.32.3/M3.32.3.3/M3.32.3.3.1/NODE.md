---
id: M3.32.3.3.1
title: EvaluatedFork canonical world projection + ported snow assertions
kind: task
status: done
owner: implementer
priority: 2
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-engine/src/lifecycle.rs"
  - "crates/slipfork-ospp-testkit/**"
  - "crates/slipfork-ospp/tests/critical_ospp.rs"
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-19T20:34"
updated: "2026-11-19T20:44"
---

# Goal
Give `crates/slipfork-forkfile` a single canonical world projection on `EvaluatedFork` (so `slipforkd` and `slipfork exec` derive env/resources/ingress from one evaluated value), delete the snow-era `Facts`/`ResourceCoordinates` bags in its favour, and port every surviving `slipfork-snow` assertion into `crates/slipfork-forkfile/tests/critical_evaluated_world.rs`.

# Context
- Owner module: `crates/slipfork-forkfile/src/ir.rs` — `EvaluatedFork` at `:289` (today services/resources only); legacy bags `FactsValue` at `:12`, `Facts` at `:22`.
- `crates/slipfork-forkfile/src/eval.rs:279` `evaluate(&FeatureSet) -> EvaluatedFork` is the only producer; `crates/slipfork-forkfile/src/resolve.rs:12` already owns per-service `resolve_env`.
- `Facts`/`ResourceCoordinates` are **deleted, not deprecated** (pre-1.0 scorched earth): remove the types and every consumer (`check.rs:58,72,136`, `props.rs:69`) — the projection below replaces them 1:1.
- New API on `EvaluatedFork`: `world_facts()`, `generated_secret_keys()`, `build_images()`, `pg_databases()`, `ingress_routes()`, `resolve_build_env(...)`, and `world_env()`.
- Gap (a): `$app_db.url`-style env has no source today — `world_facts()` + `resolve_build_env()` must supply it from the active feature set.
- Gap (b): `slipfork exec` has no defensible world-level base env — `world_env()` is documented as the **union of declared `env` groups**; state the declared precedence order explicitly in the doc comment (later group wins, as the Forkfile spec defines) so CLI and daemon agree.
- Gap (c): `[resources].{pg_databases,s3_buckets}` strategy overrides in the old `slipfork.toml` have no home — `pg_databases()` plus `slipfork-dag::ResourcePlan` (ARCHITECTURE.md:64) replace `ResourceCoordinates`.
- Porting, not copying: read `crates/slipfork-snow/tests/critical_snow.rs` and re-express each surviving assertion against `EvaluatedFork` in the new file — secret taint `!!` vs `??` precedence, feature/mode inheritance, resource ids. Never edit or delete anything under `crates/slipfork-snow/**` (owned by M3.32.3.3.3); the new test file is additive and unprotected.
- Laws: zero `std::env`/clock/`static` mutable state in the library; helpers stay inside `slipfork-forkfile`; project stays pure evaluation (no I/O).
- Keep each file <=500 LOC — if `ir.rs`/`eval.rs` grow, add `src/projection.rs` + its `mod` line in `lib.rs`.

# Acceptance
- `cargo fmt --all -- --check`
- `cargo clippy -p slipfork-forkfile --all-targets -- -D warnings`
- `cargo test -p slipfork-forkfile` (includes the new `tests/critical_evaluated_world.rs`)
- `uv run --no-project tools/guard.py --base 8ef3a5e --node M3.32.3.3.1`
- `uv run --no-project tools/critical_guard.py --base 8ef3a5e --node M3.32.3.3.1`
- `grep -rn "Facts\|ResourceCoordinates" crates/slipfork-forkfile/src` returns nothing.
- Btrfs live suites and `tools/do_vm.py sync/test` are deferred to the `M3.32.6` gate.
