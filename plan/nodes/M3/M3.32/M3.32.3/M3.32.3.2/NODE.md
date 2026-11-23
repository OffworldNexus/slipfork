---
id: M3.32.3.2
title: "wire subscribe.forkfile: EvaluatedFork + OSPP/testkit retype on slipfork-forkfile"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.3.1]
paths:
  - "crates/slipfork-wire/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-ospp-testkit/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork/src/daemon/**"
  - crates/slipfork/tests/critical_ospp_daemon.rs
  - docs/reference/ospp.md
  - docs/design/ospp.md
  - Cargo.toml
  - Cargo.lock
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-19T19:49"
updated: "2026-11-19T20:16"
---

Goal: wire subscribe payload carries forkfile: EvaluatedFork as a single field (no dual shape), protocol bumped; OSPP model/client/device_key/keys/error and ospp-testkit signers/reference_server retyped on slipfork-forkfile (Facts, Resolvers, TaintedString), loopback ref server semantics intact. Acceptance: cargo test -p slipfork-wire -p slipfork-ospp -p slipfork-ospp-testkit; clippy; fmt; make check. No RESULT.md.

# Acceptance

- `cargo test -p slipfork-wire -p slipfork-ospp -p slipfork-ospp-testkit` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
