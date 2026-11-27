---
id: M3.139
title: "Flat fork storage: <root>/.forks flat engine+layout, .forks-free checkouts, parent lineage, plan_converge wired"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.138]
paths:
  - "crates/slipfork-world/src/engine.rs"
  - "crates/slipfork-world/src/engine/**"
  - "crates/slipfork-world/src/layout.rs"
  - "crates/slipfork-world/src/layout/**"
  - "crates/slipfork-world/src/error.rs"
  - "crates/slipfork-world/src/checkout.rs"
  - "crates/slipfork-world/src/port.rs"
  - "crates/slipfork-world/src/runtime_dir.rs"
  - "crates/slipfork-world/src/ledger/**"
  - "crates/slipfork-world/src/snapshot/lease.rs"
  - "crates/slipfork-world/tests/critical_btrfs_live.rs"
  - "crates/slipfork-world/tests/ledger_converge_lab.rs"
  - "crates/slipfork-world/tests/fork_flat_live.rs"
  - Makefile
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T01:49"
updated: "2026-11-25T03:44"
---

# Goal

Every fork lands flat at `<root>/.forks/<name>` with world id `<name>--<root>` regardless of source depth (no `salut--bonjour--test1` compounding); fork checkouts never contain `.forks` or `.slipfork-store`; the immediate parent is recorded in `forks.parent_id` and `Lease.origin`; `plan_converge` is wired to `BtrfsEngine`; adopt/delete rules of §7.3–§7.4 hold. No daemon edits.

# Context

- `crates/slipfork-world/src/engine.rs:257-330` `fork()` currently derives paths from the *called* project; it must resolve the family root (`layout::project_root` `layout.rs:329`, `fork_under` `:343`) and key `project_fork_path`/`fork_world_name` off that root.
- `crates/slipfork-world/src/layout.rs:315-360` `fork_checkout`/`project_root`/`fork_under` climb only one `.forks` level today — make them climb to the root for any depth; `:364 fork_checkouts`, `:405 world_id_for_project`, `:414 ProjectScope::resolve` must become ledger-row driven (rows are the truth; shallow `read_dir` only inside converge).
- `.forks`/`.slipfork-store` invariant: `engine.rs:138-180 strip_managed_dirs` + `layout.rs:283 is_managed_entry` exist — assert it on both creation paths, including plain-directory copy `checkout.rs:24 copy_checkout` (skip managed entries) and the `ino == 2` stub unlink.
- Lineage: extend `Lease` (`crates/slipfork-world/src/snapshot/lease.rs`) with `origin: Option<String>` if absent; `BtrfsEngine::fork` must return the parent world id so the caller records `parent_id` (entity ownership stays in slice 1, do not edit `src/ledger/**`).
- `drop`: `engine.rs:386 drop_fork`, `:424 drop_world`; destroying a fork destroys only `<fork>--<root>` subvolumes + pods, never the root's `checkout_path` or `worlds/<root>`.
- Wire the pure planner: `engine.rs` supplies observed facts (checkout/lease/pod/subvolume existence) into `ledger::plan_converge`; the arithmetic stays in `src/ledger/converge.rs` (slice 1).
- Tests: extend `crates/slipfork-world/tests/critical_btrfs_live.rs:495,519,549` (currently `fork_checkouts`), new `crates/slipfork-world/tests/fork_flat_live.rs`.

# Acceptance

- `cargo test -p slipfork-world`; live: `cargo test -p slipfork-world --features live --test fork_flat_live --test critical_btrfs_live`.
- `cargo clippy -p slipfork-world --all-targets -- -D warnings`; `uv run --no-project tools/guard.py`.
- VM proof: `tools/do_vm.py ssh -- 'cd ~/slipfork && cargo test -p slipfork-world --features live --test fork_flat_live'` on a private tree → 3-deep `test1 → bonjour → salut` yields `test1/.forks/{bonjour,salut}`, `salut--test1`, zero `.forks`/`.slipfork-store` inside either checkout, `snapshot_ms < 500ms`, and a restored `plan_converge` action list for a missing checkout / orphan subvolume.
