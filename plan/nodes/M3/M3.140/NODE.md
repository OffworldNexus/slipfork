---
id: M3.140
title: "__daemon cutover: StateDb writer, ConvergeExecutor, delete worlds.json/registry, single-plan drop, ps lineage field"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.139, M3.137]
paths:
  - "crates/slipfork/src/daemon/**"
  - crates/slipfork/tests/critical_daemon.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/daemon_skew_live.rs
  - "crates/slipfork/tests/support/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T01:49"
updated: "2026-11-25T11:20"
---

# Goal

Cut `__daemon` over to the ledger as the only world registry: it opens the `StateDb` writer after `flock`, runs `Migrator::up` before `UnixListener::bind`, adds `ConvergeExecutor` on start and before `up`/`ps`, deletes `worlds.json` + `daemon/registry.rs` + `Fingerprint`, makes `drop` one upfront plan frame with child splicing, and adds the wire `lineage` field slice 4 renders.

# Context

- `crates/slipfork/src/daemon/mod.rs:107-163` `Daemon::start`: acquire `flock` (`daemon/lock.rs:29`) **before** opening the DB, then `StateDb::open_writer`, `Migrator::up`, then bind. Legacy load/store lives at `:147-158`.
- `crates/slipfork/src/daemon/registry.rs:1-135` (`worlds.json`, `RegistryEntry`, `Fingerprint`, `store_fingerprint`) is deleted; one-transaction `worlds.json` import (§7.6) plus its unlink is the only surviving code path.
- Call sites to unwind: `daemon/lifecycle/core.rs:19,125,140,159`; `lifecycle/up.rs:16,125,139`; `lifecycle/fork.rs:320,492`; `lifecycle/logs/plan.rs:16`; `daemon/boot.rs:11`; `lifecycle/ps.rs:190,393,450` (`fork_checkouts` + `RegistryEntry`) → `ForkLedger::list`.
- New `crates/slipfork/src/daemon/lifecycle/converge.rs` implements `ConvergeExecutor` for every §7.4 action, `reap_world_tree` (`slipfork-world/src/reap.rs`), scoped strictly to roots in `state.db`.
- `drop`: `lifecycle/down.rs` + `lifecycle/plan.rs` + `frames.rs` — one `plan` frame with `[1/N]..[N/N]` (pod rows + `fork snapshots`), no `PrepFrame` double-count; children reparented before the `worlds` delete.
- Add `lineage: Vec<String>` to `PsWorld` (`daemon/ps.rs:16-60`) and a `broken` state (`PsWorldState` `daemon/ps.rs:90`); slice 4 renders only.
- `crates/slipfork/src/boot/**` consumers of registry entries must read ledger rows during reconciliation.
- Legacy-id sweep expected: `PrepFrame::fixed`, `fork_checkouts`, `world_id_for_project` have **no** live callers outside `lifecycle/ps.rs:190,450` and `layout.rs` today — grep before deleting and report any residue instead of inventing a shim.

# Acceptance

- `cargo test -p slipfork`; `cargo clippy --workspace --all-targets -- -D warnings`; `uv run --no-project tools/guard.py`.
- Grep gate: zero `worlds.json`, `RegistryEntry`, `store_fingerprint`, `Fingerprint`, `fork_checkouts`, `world_id_for_project` in `crates/slipfork/src/**` (the transient importer excepted).
- VM proof: `tools/do_vm.py ssh` on a private tree → `SIGKILL` after `begin_fork` leaves zero ghosts on next `ps`; `rm -rf test1/.forks/merci` reaps its pods before `ps` returns; `slipfork drop bonjour -f` emits exactly one `plan` frame with monotonic `[1/N]..[N/N]`.
