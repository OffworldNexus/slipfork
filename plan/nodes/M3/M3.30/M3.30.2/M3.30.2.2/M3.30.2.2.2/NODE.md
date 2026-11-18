---
id: M3.30.2.2.2
title: PostgresProvisioner + OsppSupervisor + PairingStore cohesive structs over the new daemon modules
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.2.2.1]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T00:31"
updated: "2026-11-19T00:31"
---

# Goal
Slice B of `plan/nodes/M3/M3.30/M3.30.2/M3.30.2.2/NODE.md`: introduce the cohesive structs.

# Work
- `daemon/postgres.rs` `PostgresProvisioner`: `restore_base`/`ensure_database`/`psql`/`pg_restore` orchestration over the engine + `PostgresReady` policy.
- `daemon/ospp_supervisor.rs` `OsppSupervisor`: OSPP client task, reconnect, `env_updated`/`dump_ready` handling.
- `daemon/pairing_store.rs` `PairingStore`: persist public `{ endpoint, account, machine }` to `<state_dir>/ospp.json` at `0600` atomically on successful `slipfork auth`; `bin/slipforkd.rs` loads it when `--ospp-endpoint` is absent. Zero secrets/tokens/private keys/claim material on disk; real-disk test asserts `0600`, exact public JSON fields, boot round-trip, and no secret bytes.

# Constraints
No behavior/diagnostic change; cohesive structs `<= 500` LOC; files `<= 560`; no new traits; no globals; no aliases. No RESULT.md.

# Acceptance
- `for f in crates/slipfork/src/daemon/*.rs; do test $(wc -l < "$f") -le 560 || echo "TOO LONG $f"; done`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test daemon_unit_install`
