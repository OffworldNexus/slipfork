---
id: M3.138
title: "ForkLedger store: sea-orm state.db entities, migrations, StateDb writer/reader, pure plan_converge"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/src/ledger/**"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/Cargo.toml"
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-world/tests/ledger_lab.rs"
  - "crates/slipfork-world/tests/ledger_converge_lab.rs"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T01:49"
updated: "2026-11-25T03:21"
---

# Goal

Add `slipfork-world/src/ledger/` — the single `state.db` store (§7.1–§7.2): sea-orm entities for `worlds`/`forks`, `m0001_create_ledger`, a fully synchronous `StateDb` (private `current_thread` runtime, `flock` single writer, WAL/`synchronous=FULL`/`foreign_keys`/`busy_timeout`, `Migrator::up` before serving), the `ForkLedger` row API, and a **pure** `plan_converge`. Zero daemon wiring, zero CLI.

# Context

- Root `Cargo.toml:24-27` `[workspace.dependencies]` has no DB crates: add `sea-orm` (`default-features=false`, `["sqlx-sqlite","runtime-tokio","macros"]`) and `sea-orm-migration` (`["sqlx-sqlite","runtime-tokio"]`) there; `crates/slipfork-world/Cargo.toml:6-27` inherits via `.workspace = true`.
- Module registration is `crates/slipfork-world/src/lib.rs:26-48` (see `pub mod snapshot;`); crate re-exports live at `:66-90`.
- Schema must match `specs/03-under-the-hood.md` §7.2 verbatim: `worlds` (`id` PK, `checkout_path` UNIQUE, `world_name`, `store_root`, `state` `WorldState` active-enum + `Table::check(is_in)`, `supervisor_pid`, `pods_dir`, `started_at_ms`, `problem`, `updated_at_ms`, `UNIQUE(store_root,world_name)`, partial `idx_worlds_active_name` over `Creating..=Deleting`); `forks` (`world_id` PK FK CASCADE, `root_id` FK CASCADE `idx_forks_root`, `parent_id` FK RESTRICT `idx_forks_parent`, `name`, `branch` default `""`, `created_at_ms`, `UNIQUE(root_id,name)`).
- Reuse existing name rules instead of re-parsing: `WorldName::parse`/`ForkName::parse` `crates/slipfork-world/src/layout.rs:215-276`, `<fork>--<root>` builder `layout.rs:157`; enforce `<name>--<root> <= 63` bytes and `control.sock < 108` bytes inside `begin_fork` before any ioctl.
- `plan_converge` is pure: input = owned rows + observed facts (`checkout_exists`, `world_data_exists`, `supervisor_alive`, pod dirs, untracked `.forks` names, orphan `worlds/<fork>--<root>*` names), output `Vec<ConvergeAction>` covering every §7.4 row. It touches no disk — slice 2 feeds the facts from `BtrfsEngine`.
- Do **not** import the legacy `worlds.json`/`RegistryEntry`/daemon `WorldState`; deleting those is slice 3.
- Tests: new `crates/slipfork-world/tests/ledger_lab.rs` on real disk via `tempfile::Builder::new().tempdir_in("/var/tmp")` (never tmpfs `/tmp`), no mocks. Delete 0%-covered fallback code rather than testing it.

# Acceptance

- `cargo test -p slipfork-world` — lab covers `Migrator::up` idempotency, concurrent `open_reader` while the writer holds the pool, unknown/pending migration fail-closed (`next: upgrade slipfork` / `next: slipfork up`), transactional `delete_world_row` splice, `UNIQUE` + partial-index conflicts, and the full `plan_converge` state matrix.
- `cargo clippy -p slipfork-world --all-targets -- -D warnings`; `uv run --no-project tools/guard.py`.
- VM proof (real disk, no Btrfs needed): `tools/do_vm.py ssh -- 'cd ~/slipfork && cargo test -p slipfork-world --test ledger_lab'` → `state.db` mode `0600`, `-wal`/`-shm` present, absent DB yields an empty reader.
