---
id: M3.44.4
title: "Reactive tokio boot DAG, per-node elapsed, world_epoch stamp & zero-rebuild <500ms fork"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.44.3]
paths:
  - "crates/slipfork-world/src/boot/**"
  - "crates/slipfork-world/src/supervisor/**"
  - "crates/slipfork-world/src/bin/world_init/**"
  - crates/slipfork/src/world/up.rs
  - "crates/slipfork/src/world/up/**"
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork/src/daemon/lifecycle/**"
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - "crates/slipfork-world/tests/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T20:17"
---

# Goal
Replace phase barriers with a true reactive `tokio` DAG (every node starts the moment its own upstream edges finish, reporting `node_start.elapsed()`), and persist a `world_epoch` stamp so `slipfork fork` performs zero image rebuilds, zero `@<tool>` installs, and zero redundant `before-start`/`pre-deploy` runs (< 500ms target, < 1.0s hard ceiling).

# Context
- `crates/slipfork-world/src/boot/schedule.rs` (227 LOC) emits waves today; rewrite as edge-driven `tokio::JoinSet` (spawn on in-degree 0, release dependents as each completes, zero barriers, deterministic error fan-out/cancel). `crates/slipfork-world/src/boot/plan.rs` (453) splits into `boot/plan.rs` (graph build only, <= 300) + `boot/graph.rs` (node/edge types + in-degree bookkeeping, <= 250); `boot/mod.rs` re-exports.
- `crates/slipfork-world/src/boot/stamp.rs` (72) grows into `boot/stamp.rs` (read/write/atomic tmp+rename) + `boot/stamp/epoch.rs`: `<world>/.slipfork-world-stamp.json` records resolved image subvolume ids, `@<tool>` install digests, and `pre-deploy`/`before-start` input stamps plus `world_epoch`; the PG sibling `<world>/pg/.slipfork-postgres-stamp.json` carries the paired epoch. Epoch match → skip image rebuild/pull/context walk and skip hooks (0 re-runs); mismatch → force hook re-execution. Never trust a stamp without the paired epoch.
- Per-node timing: capture `Instant` at spawn and emit `{kind, name, state, duration_ms}` over the existing daemon event stream consumed by `crates/slipfork/src/daemon/lifecycle/{up,fork}.rs`, plus live `Dockerfile` `RUN` / hook lines from `crates/slipfork-world/src/bin/world_init/stream.rs`.
- Split hazards before editing: `crates/slipfork-world/src/bin/world_init/stream.rs` is 488 LOC → `bin/world_init/stream/lines.rs` (framing, <= 250) + `stream/tail.rs` (rolling tail + backpressure, <= 250). `crates/slipfork-world/src/supervisor/mod.rs` (450) → `supervisor/loop.rs` (<= 250) leaving `mod.rs` <= 350; `supervisor/boot.rs` (344) stays <= 350. `crates/slipfork/src/world/up.rs` is exactly 500 → `world/up.rs` (orchestration <= 300) + `world/up/nodes.rs` (node wiring/event mapping <= 250).
- DI: `tokio::runtime::Handle` is constructed by the `__supervise` composition root and threaded in as a value; `spawn_blocking` for Btrfs/subprocess/git. No `std::env` reads, `OnceLock`, `LazyLock`, or mutable statics in `slipfork-world` library code.
- Out of scope: rendering (`M3.44.6`) — emit structured events only; the `clap` surface (`M3.44.5`). Zero mocks; real Btrfs/`crun`/git.

# Acceptance
- `cargo test -p slipfork --test critical_fork_live` green: real Btrfs lab, parent with seeded PG rows + Redis keys + S3 objects + `@uv`/`@pnpm` + image + route; asserted `elapsed < Duration::from_secs(1)`; matching epoch → 0 rebuilds / 0 hook re-runs; mismatched epoch → hooks re-run; fork writes isolated; `drop` refuses dirty checkout without `-f`.
- `cargo test -p slipfork --test critical_world_journey` green (reactive ordering, per-node elapsed rows, live `RUN`/hook lines).
- `cargo test -p slipfork-world` and `cargo test -p slipfork-world --test critical_btrfs_live` green.
- No `boot/`, `supervisor/`, `world/up*`, or `world_init/stream*` file exceeds 500 LOC; `cargo clippy --workspace --all-targets -- -D warnings`; `uv run --no-project tools/guard.py` exit 0.
