---
id: M3.30.2
title: "Split 3 god-files into cohesive structs, fix edge layering, replace hand-rolled HTTP/NDJSON wheels, persist ospp.json"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1]
paths: [Cargo.toml, Cargo.lock, "crates/**", ARCHITECTURE.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-18T22:19"
---

# Goal
Break the three god-files into cohesive OOP structs (`<= ~500` LOC each, no 6-arg free functions, no `*Inputs` bags), restore `slipfork-edge` as a pure `.localhost` proxy crate, replace hand-rolled HTTP/NDJSON with `ureq`/`serde_json`, and give OSPP pairing durable `ospp.json` state.

# Context
Audit-verified anchors (HEAD). Do NOT re-research.

## 1. `slipfork-edge` layering
- `slipfork-edge` must stay a pure `.localhost` HTTP/WebSocket reverse proxy + systemd socket activation crate (`src/ingress.rs`, `src/registry.rs`, `src/activation.rs`, `src/relay.rs`).
- `crates/slipfork-edge/src/daemon.rs` is `999` LOC and owns `DaemonControl` IPC request/reply/view-model types → move those types to a dedicated `control` module in the `slipfork` crate (or `slipfork-world` if they are shared without CLI tables); keep `slipfork-edge` free of CLI tables and world internals.
- `crates/slipfork-edge/src/unix_socket.rs:14` imports `slipfork_engine::world_init` internals — remove that edge (re-export the socket path/framing from the control module instead).

## 2. Replace hand-rolled wheels
- `crates/slipfork-ospp/src/client.rs:726-832`: `http_get` / `http_body` / `parse_http_url` / `tls_stream` hand-rolled HTTP/1.1 client + URL splitter → `ureq` (rustls backend) + `url`.
- `crates/slipfork-ospp/src/wire.rs` (`157` LOC) hand-rolled NDJSON `FrameReader` → `serde_json::Deserializer::from_reader` streaming, keeping the exact wire contract (STP v1 framing lives in the owning module).
- `crates/slipfork/src/main.rs:462` `write_file_atomic` → `tempfile::NamedTempFile::persist` (0600, same-dir temp).

## 3. God-file splits (cohesive structs)
### `crates/slipfork/src/daemon.rs` (5,580 LOC) → `crates/slipfork/src/daemon/{mod,boot,postgres,ospp_supervisor,pairing_store}.rs`
- `WorldSession`: owns world lifecycle (boot, port assignment, status, teardown).
- `PostgresProvisioner`: owns `ensure_database` / `psql` / `pg_restore` orchestration.
- `OsppSupervisor`: owns the OSPP client task, reconnect, and `env_updated` / `dump_ready` handling.
- `PairingStore`: persists public `{ endpoint, account, machine }` to `<state_dir>/ospp.json` with `0600` when `slipfork auth <wss-url>` succeeds; `slipforkd` loads it automatically at boot when `--ospp-endpoint` is absent. Zero secrets on disk — no token, no private key, no claim material.
### `crates/slipfork-engine/src/btrfs_engine.rs` (4,150 LOC) + `crates/slipfork-engine/src/service.rs` (22 loose PID functions)
- `BtrfsWorldEngine` (subvolume orchestration), `OciContainerRuntime` (`crun`), `SupervisedProcess` (owns `setsid` + `-pgid` group stop & reap, replacing the 22 free functions in `service.rs`).
### `crates/slipfork/src/main.rs` (1,676 LOC, 54 free fns)
- `< 250`-LOC Composition Root (only place reading env/proc/sockets) + cohesive `CliApp` struct (`output_mode`, `layout`, `ui`) + `crates/slipfork/src/commands/*.rs` modules.
- Collapse `crates/slipfork-vcs/src/tool_command.rs` (12 free fns) into a cohesive `VcsCli` struct.

# Deliverables
- Every touched file `<= ~500` LOC (composition root `< 250`); `wc -l` gate in acceptance.
- `ARCHITECTURE.md` updated: `slipfork-edge` purity statement, `slipfork` `control` module, `PairingStore` state file contract (`ospp.json`, `0600`, public fields only).
- `Cargo.toml`/`Cargo.lock`: `ureq`, `url`, `tempfile` added where needed; dead deps removed.

# Constraints
- `slipfork-edge` gains zero dependencies on `slipfork-engine` internals or CLI tables.
- No new traits: cohesive structs with methods, wired at the composition root.
- Keep the public wire contract byte-identical (OSPP v1 + STP v1 framing); this is a refactor, not a protocol change.
- No behavior regressions in `status` / `ps` / `logs` / `exec` / `restart` / `fork` / `drop`.

# Acceptance
- `test $(wc -l < crates/slipfork/src/main.rs) -lt 250`
- `for f in crates/slipfork/src/daemon/mod.rs crates/slipfork/src/daemon/boot.rs crates/slipfork/src/daemon/postgres.rs crates/slipfork/src/daemon/ospp_supervisor.rs crates/slipfork/src/daemon/pairing_store.rs crates/slipfork-engine/src/btrfs_engine.rs crates/slipfork-engine/src/service.rs; do test $(wc -l < "$f") -le 560 || echo "TOO LONG $f"; done`
- `! grep -rn "slipfork_engine::world_init\|slipfork_engine::" crates/slipfork-edge/src/`
- `! grep -rn "fn parse_http_url\|fn http_body\|fn tls_stream\|struct FrameReader" crates/slipfork-ospp/src/`
- `! grep -rn "fn write_file_atomic" crates/slipfork/src/main.rs`
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-ospp -p slipfork-edge -p slipfork-world -p slipfork-vcs -p slipfork-engine --lib`
- `cargo test -p slipfork --lib`
