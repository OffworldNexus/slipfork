---
id: M3.30.2.1
title: "Edge purity + canonical wheels: control module, relay out of engine, ureq/url, serde_json NDJSON, atomic write"
kind: epic
status: active
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-edge/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-engine/src/relay.rs"
  - "crates/slipfork-world/**"
  - "crates/slipfork/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T00:03"
updated: "2026-11-19T00:06"
---

# Goal
Section 1 + 2 of `plan/nodes/M3/M3.30/M3.30.2/NODE.md` (read it): make `slipfork-edge` a pure `.localhost` proxy crate and replace hand-rolled wheels.

# Work
- Move `DaemonControl` IPC request/reply/view-model types out of `crates/slipfork-edge/src/daemon.rs` into a `control` module in the `slipfork` crate (no CLI tables in `slipfork-edge`).
- Remove `crates/slipfork-edge/src/unix_socket.rs:14` importing `slipfork_engine::world_init`; re-export socket path/framing from the control module instead.
- Relocate the shared relay (`slipfork-engine::relay::{Duplex, pipe}`, already used by edge ingress/forward) to a neutral canonical owner (e.g. `slipfork-world` or `slipfork-edge`) so `slipfork-edge` gains ZERO `slipfork-engine` dependency.
- `crates/slipfork-ospp/src/client.rs` hand-rolled `http_get`/`http_body`/`parse_http_url`/`tls_stream` -> `ureq` (rustls) + `url`; keep the OSPP v1 wire contract byte-identical.
- `crates/slipfork-ospp/src/wire.rs` hand-rolled NDJSON `FrameReader` -> `serde_json::Deserializer::from_reader` streaming, same framing contract.
- `crates/slipfork/src/main.rs` `write_file_atomic` -> `tempfile::NamedTempFile::persist` (same dir, `0600`).

# Constraints
No behavior/wire regression; edge keeps zero CLI tables and zero engine internals; no aliases. No RESULT.md.

# Acceptance
- `! grep -rn "slipfork_engine::" crates/slipfork-edge/src/`
- `! grep -rn "fn parse_http_url\|fn http_body\|fn tls_stream\|struct FrameReader" crates/slipfork-ospp/src/`
- `! grep -rn "fn write_file_atomic" crates/slipfork/src/main.rs`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-ospp -p slipfork-edge -p slipfork-world`
- `cargo test -p slipfork --lib`
