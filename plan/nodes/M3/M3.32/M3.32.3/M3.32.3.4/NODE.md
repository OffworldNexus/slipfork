---
id: M3.32.3.4
title: "Capture-time ts_ms + concurrent logs -f fan-in with restart divider"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.3.2]
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-wire/**"
  - crates/slipfork/src/logs.rs
  - crates/slipfork/src/daemon/protocol.rs
  - crates/slipfork/src/daemon/world.rs
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T19:49"
updated: "2026-11-19T19:49"
---

Goal: stamp ts_ms at capture in the engine log reader and LogChunk; bump the wire LogsRequest/reply to NDJSON ts_ms/target/stream/text; rewrite DaemonService::logs in the daemon into a thread-per-source concurrent fan-in (services and hooks together); add a restart epoch divider event (dimmed restarted-at) in daemon/world.rs; renderer rules: chronological merge, dimmed divider, one named target raw unless -v. Acceptance: cargo test -p slipfork-engine -p slipfork-wire -p slipfork; concurrency interleave test; divider-on-restart test; raw single-target test; clippy/fmt; VM sync+test exit 0. No RESULT.md.

# Acceptance

- Capture-time `ts_ms` present end to end; NDJSON `{"ts_ms","target","stream","text"}`.
- Tests prove concurrent interleaving across services/hooks, the dimmed restart divider, and raw single-target output.
- `cargo test -p slipfork-engine -p slipfork-wire -p slipfork` exits 0; clippy/fmt clean.
