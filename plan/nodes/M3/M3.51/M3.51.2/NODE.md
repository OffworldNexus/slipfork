---
id: M3.51.2
title: "Atomic fork rollback: stop supervisors, drop worlds.json entry, purge snapshots on failure"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/src/daemon/boot.rs
  - crates/slipfork/tests/critical_fork_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T17:24"
updated: "2026-11-22T17:47"
---

# Goal

A `fork` that fails mid-boot must roll back atomically: no surviving supervisor (and no sibling pods it started), no `worlds.json` entry, no `.forks/<name>` or `worlds/<fork>--<world>` remnant — so a retry with the same name always works.

# Context

- `crates/slipfork/src/daemon/lifecycle/fork.rs:135-150` — `discard_fork(&self, prepared)` only runs `RuntimePods::purge` then `BtrfsEngine::drop_fork`. It never calls `stop_world`, never removes the fork key from `self.worlds`/`worlds.json`, so a failed boot leaks a live supervisor + registered world while the snapshots are unlinked.
- Change the signature to `discard_fork(&mut self, prepared: &PreparedFork)` and, BEFORE the purge/drop:
  1. derive the registry key exactly as `begin_up`/`reserve_fork` does (same `key_of(&project)`/`fork_world_name` helper — do not invent a second naming scheme),
  2. `self.stop_world(&fork_key, …)` (ignore not-found on the pre-registration path),
  3. `self.worlds.remove(&fork_key)`,
  4. `self.persist()`.
  Then purge pods and drop the snapshots as today. Hold no lock across `stop_world`; return the first error only if it matters to the caller.
- Callers: `crates/slipfork/src/daemon/lifecycle/fork.rs:126-130` (`begin_up` failure, nothing registered yet — must still compile with `&mut self`) and `crates/slipfork/src/daemon/mod.rs:413-418` (`if let Err(failure) = session.run(…)` → `self.core()?` then `core.discard_fork(&prepared)`). Route the latter through the mutable core accessor used elsewhere in `mod.rs` for `begin_fork`.
- `PreparedFork` fields available: `parent`, `world`, `world_name`, `fork`, `fork_name`, `project` (see `fork.rs` `PreparedFork` definition and `begin_fork` construction) — use them, no new params.
- E2E: extend `crates/slipfork/tests/critical_fork_live.rs` (real Btrfs + crun lab, disk `tempfile`, never RAM `/tmp`). Force a failure after sibling services have started (a `pre-deploy` hook or `pg` that exits non-zero), then assert: (a) no pid from the fork's supervisor/pods answers, (b) `worlds.json` has no `<parent>--<fork>`/fork entry, (c) `<project>/.forks/<name>` does not exist, (d) an immediate retry `fork` with the same name succeeds.

# Acceptance

- `cargo fmt --all -- --check`
- `cargo clippy -p slipfork --all-targets -- -D warnings`
- `cargo test -p slipfork --test critical_fork_live` — new `fork_rollback_*` case covering (a)-(d)
- `cargo test -p slipfork --lib daemon` (and `--lib` lifecycle fakes stay compile-clean)
- `git grep -n "discard_fork(&prepared)" -- crates/` shows only the corrected call sites
