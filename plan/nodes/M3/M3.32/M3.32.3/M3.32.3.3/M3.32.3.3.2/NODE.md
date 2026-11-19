---
id: M3.32.3.3.2
title: "Retype slipforkd on EvaluatedFork, VCS project identity, delete slipfork.toml loader"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.3.3.1]
paths: ["crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T20:34"
updated: "2026-11-19T20:34"
---

# Goal
Run `slipforkd` end-to-end on `slipfork_forkfile::EvaluatedFork`: features instead of modes, `resolve_env`/`world_env` instead of expression scanning, projection-derived resource ids and ingress; project identity from `Forkfile project` or VCS origin; delete the `slipfork.toml` loader and the snow bridge internals. Every file <=500 LOC.

# Context
- `crates/slipfork/src/daemon/` LOC hot spots (from `wc -l`): `world.rs:560` (must split below 500), `types.rs:498`, `protocol.rs:477`, `resolve.rs:472`, `env.rs:425`, `postgres.rs:401`, `ospp_supervisor.rs:378`, `mod.rs:360`, `service.rs:350`, `backing.rs:180`, `vault.rs:78`, `base.rs:67`. Shims to delete: `snow_bridge.rs:63`, `snow_resolve.rs:58`.
- Also retype `crates/slipfork/src/daemon/vaults.rs` and `process.rs` under `crates/slipfork/src/daemon/commands/` — verify layout first with `find crates/slipfork/src -name vaults.rs -o -name process.rs`; keep the same module responsibility if names moved.
- Source of truth from M3.32.3.3.1: `crates/slipfork-forkfile/src/ir.rs:289` `EvaluatedFork` with `world_facts`, `generated_secret_keys`, `build_images`, `pg_databases`, `ingress_routes`, `resolve_build_env`, `world_env`; `crates/slipfork-forkfile/src/resolve.rs:12` is the ONLY env resolver. The daemon must never scan `${{` delimiters or re-derive resource ids.
- Delete the `slipfork.toml` loader outright (no alias, no `#[deprecated]`): resources come from `EvaluatedFork::pg_databases()` + `slipfork-dag::ResourcePlan`.
- Project identity: `crates/slipfork-vcs` `RepoRootResolver` / `ProjectSlugOverride` (ARCHITECTURE.md:32,69) — `Forkfile project <slug>` wins, else verbatim Git `origin`.
- Compile-unit constraint: `slipfork_edge::DaemonControl` still requires `snow_resolve` until M3.32.3.3.3. So delete the real snow machinery here (`snow_bridge.rs`, `snow_resolve.rs`, all `slipfork_snow` imports, the `snow` CLI subcommand, `protocol.rs` snow plumbing) and leave exactly ONE thin adapter `snow_resolve` body in `protocol.rs` delegating to `EvaluatedFork::resolve_env`, commented `// removed in M3.32.3.3.3`. No `Unavailable`/degraded stubs beyond that.
- Do NOT edit `crates/slipfork/tests/**` or `crates/slipfork/tests/support/**` — M3.32.3.3.3 ports those fixtures onto Forkfile manifests.
- Laws: single composition root stays in `crates/slipfork/src/main.rs`; no new `OnceLock`/`static` state; cohesive structs with methods, no 6-arg free functions; log/UX rules unchanged (no `\x1b[` under `NO_COLOR`/`--json`).

# Acceptance
- `cargo fmt --all -- --check`
- `cargo clippy -p slipfork --all-targets -- -D warnings`
- `cargo test -p slipfork --lib` (in-crate unit tests; live CLI journeys belong to M3.32.3.3.3)
- `uv run --no-project tools/guard.py --base 8ef3a5e --node M3.32.3.3.2`
- `uv run --no-project tools/critical_guard.py --base 8ef3a5e --node M3.32.3.3.2`
- `wc -l crates/slipfork/src/daemon/*.rs` — every file <=500.
- `grep -rn "slipfork_snow\|snow\.yml\|slipfork\.toml" crates/slipfork/src` returns nothing.
- Btrfs live suites and `tools/do_vm.py sync/test` are deferred to the `M3.32.6` gate.
