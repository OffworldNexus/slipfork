---
id: M3.134
title: "ForkLedger: one state DB for worlds and forks, flat envs with lineage, deterministic convergence"
kind: epic
status: done
priority: 3
depends_on: [M3.120]
paths:
  - "crates/slipfork-world/src/ledger/**"
  - "crates/slipfork-world/src/engine.rs"
  - "crates/slipfork-world/src/layout.rs"
  - "crates/slipfork-world/src/checkout.rs"
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/world/**"
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/commands/**"
  - "specs/02-cli-ux.md"
  - "crates/slipfork/tests/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T22:52"
updated: "2026-11-25T12:31"
---

# Contract

`specs/03-under-the-hood.md` §7 (`§7.1`–`§7.6`) — read it, it is the authority.

- **One state store:** bundled `rusqlite` DB at `<state_dir>/state.db` (`0600`, `journal_mode=WAL`, `synchronous=FULL`, transactional DDL via `PRAGMA user_version`), single writer = `__daemon`; offline CLI readers open `SQLITE_OPEN_READ_ONLY`. `state.db` owns **only** `worlds` and `forks` — it deletes `worlds.json`, `daemon/registry.rs`, `.slipfork/world.json`, `PrepFrame::fixed`, `fork_checkouts`, `world_id_for_project`.
- **No second source of truth:** `SnapshotStore` (`<key>.json`) keeps sealed nodes; `Lease` keeps world heads/effects; `LogStore` keeps runs. `M3.120` slice 4 deletes the legacy stamps and `worlds-fingerprints.json`.
- **Fork any environment, flat storage:** every fork lands at `<root>/.forks/<name>` (`<name>--<root>` world) regardless of source; `forks.parent_id` + `Lease.origin` record the immediate parent (`salut ← bonjour ← test1`); no depth cap. `BtrfsEngine` enforces zero `.forks`/`.slipfork-store` inside any checkout.
- **Convergence:** pure `ForkLedger::plan_converge` (in `slipfork-world`) + `ConvergeExecutor` (`__daemon`) idempotently settle `creating`/`deleting` crashes, missing checkouts (reap live pods → `broken`), missing leases, orphan pod trees, legacy `.forks` stubs, untracked checkouts, orphan `worlds/<fork>--<root>*` subvolumes.
- **`drop`** splices direct children behind one upfront `plan` frame `[1/N]..[N/N]`; **`ps`** renders lineage + broken entries with exactly one remedy.

# Slices (planner: 4 coarse vertical slices, ≤500 LOC files)

1. `slipfork-world` `ledger/` (schema + migrations + `worlds`/`forks` rows + `plan_converge` pure) with real-disk lab tests.
2. Flat `engine.rs` + `layout.rs` fork creation (no nesting), `.forks`-free checkouts, `plan_converge` wired.
3. `__daemon` cutover: `state.db` writer, `ConvergeExecutor`, delete the legacy world registries; `drop` single plan.
4. `ps` lineage + `specs/02` sync + live VM proof of every ghost state from the owner's session.

# Acceptance

- Owner's exact states converge: fork-of-fork flat (`salut ← bonjour ← test1`), `drop` one plan total, no phantom `checkout missing`, no nested `.forks` in a checkout, half-created fork reaped.
- Live Btrfs VM (private tree, never `do_vm.py sync`, never `~/demo`); full workspace + `make check`; Triad per slice.
