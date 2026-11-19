---
id: M3.30.4.4
title: "Edge live suite: replace StubWorldEngine with KernelBtrfsDriver in require_btrfs critical_edge_live"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.4.2]
paths: ["crates/slipfork-edge/tests/**", Makefile, "crates/slipfork-edge/Cargo.toml"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T02:24"
updated: "2026-11-19T02:24"
---

# Goal
Delete the last test double, `StubWorldEngine` (`crates/slipfork-edge/tests/critical_edge.rs`), by adding a stub-free `critical_edge_live.rs` that boots a real `BtrfsEngine` under `require_btrfs` and carries the registry/ingress routing assertions; register it in `rs-test-live`. Keep pure framing/proxy tests in `critical_edge` with no engine at all if possible.

# Acceptance
- no `StubWorldEngine` under crates/
- `make -n rs-test-live` lists critical_edge_live
- `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `make rs-test` exit 0 locally
