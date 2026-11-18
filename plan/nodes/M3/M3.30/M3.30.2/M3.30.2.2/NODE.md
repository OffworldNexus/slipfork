---
id: M3.30.2.2
title: Split daemon.rs into cohesive WorldSession/PostgresProvisioner/OsppSupervisor/PairingStore modules
kind: epic
status: active
priority: 3
depends_on: [M3.30.2.1]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T00:03"
updated: "2026-11-19T00:31"
---

# Goal
Section 3 (`daemon.rs`) of `plan/nodes/M3/M3.30/M3.30.2/NODE.md`: split the god-file into `crates/slipfork/src/daemon/{mod,boot,postgres,ospp_supervisor,pairing_store}.rs` with cohesive structs `<= ~500` LOC.

# Work
- `WorldSession`: world lifecycle (boot, port assignment, status, teardown).
- `PostgresProvisioner`: `ensure_database`/`psql`/`pg_restore` orchestration.
- `OsppSupervisor`: OSPP client task, reconnect, `env_updated`/`dump_ready` handling.
- `PairingStore`: persist public `{ endpoint, account, machine }` to `<state_dir>/ospp.json` `0600` on successful `slipfork auth`, loaded by `slipforkd` at boot when `--ospp-endpoint` is absent. Zero secrets/tokens/private keys/claim material on disk.
- Preserve every diagnostic/`next:` string and the control-socket protocol.

# Constraints
No behavior regressions in status/ps/logs/exec/restart/fork/drop; no new traits; no globals. No RESULT.md.

# Acceptance
- `for f in crates/slipfork/src/daemon/mod.rs crates/slipfork/src/daemon/boot.rs crates/slipfork/src/daemon/postgres.rs crates/slipfork/src/daemon/ospp_supervisor.rs crates/slipfork/src/daemon/pairing_store.rs; do test $(wc -l < "$f") -le 560 || echo "TOO LONG $f"; done`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test daemon_unit_install`
