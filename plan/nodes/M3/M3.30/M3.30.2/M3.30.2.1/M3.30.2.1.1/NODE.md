---
id: M3.30.2.1.1
title: "New slipfork-wire crate: DaemonControl types + socket framing + neutral relay; finish wheel swap"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - "crates/slipfork-wire/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork-engine/src/{relay.rs"
  - lib.rs
  - "bin/slipfork-world-init.rs}"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T00:06"
updated: "2026-11-19T00:19"
---

# Goal
Resolve the `slipfork -> slipfork-edge` cycle from `M3.30.2.1` by introducing a neutral `slipfork-wire` crate, and land the already-green wheel swaps.

# Work
- Create `crates/slipfork-wire` (workspace member): `control` module with the `DaemonControl` request/reply/view-model types + control-socket path/framing (moved out of `crates/slipfork-edge/src/daemon.rs`/`unix_socket.rs`), and the neutral relay primitives (`Duplex`, `pipe`) moved out of `slipfork-engine`. No I/O policy, no CLI tables, no world internals.
- `slipfork-edge` depends on `slipfork-wire` (not on `slipfork`); `slipfork` depends on wire for control types. Remove the `slipfork_engine::world_init` internals import from `edge/src/unix_socket.rs`.
- Keep the already-applied, green wheel swaps in this diff: `ureq` + `url` in `slipfork-ospp/src/client.rs`, `tempfile::NamedTempFile` atomic write in `daemon_unit_install.rs`, pruned `rustls`/`webpki-roots`.
- Update `ARCHITECTURE.md` for the new crate and where the control types/relay live.

# Constraints
No behavior/wire change; no new traits; no CLI tables in edge; no aliases. No RESULT.md.

# Acceptance
- `! grep -rn "slipfork_engine::" crates/slipfork-edge/src/` (control/relay no longer reach into engine)
- `! grep -rn "slipfork::" crates/slipfork-edge/Cargo.toml` and no `slipfork-edge -> slipfork` dep
- `! grep -rn "fn parse_http_url\|fn http_body\|fn tls_stream" crates/slipfork-ospp/src/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-wire -p slipfork-ospp -p slipfork-edge`
- `cargo test -p slipfork --lib`
