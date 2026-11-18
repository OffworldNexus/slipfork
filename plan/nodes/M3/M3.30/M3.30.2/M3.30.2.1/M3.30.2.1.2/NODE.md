---
id: M3.30.2.1.2
title: "Extract WorldEngine port + world value types to slipfork-world; edge has zero engine dependency"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.2.1.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - "crates/slipfork-world/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork/**"
  - "crates/slipfork-testkit/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T00:06"
updated: "2026-11-19T00:26"
---

# Goal
Second half of `M3.30.2.1`: the `slipfork-edge` crate must name no `slipfork_engine::` type at all.

# Work
- Move the `WorldEngine` port trait and the value types edge needs (`WorldHandle`, `ExecEnv`, `LogChunk`, `StreamChunk`, `WorldEntryCommand`, lineage/status values) into `slipfork-world` (identity/values/ports, no I/O).
- `slipfork-engine` implements the moved trait; `slipfork-edge` (`registry.rs`, `forward.rs`, `ingress.rs`) and its test harness import only `slipfork-world` + `slipfork-wire`; remove the `slipfork-engine` dependency from `crates/slipfork-edge/Cargo.toml`.
- Update `ARCHITECTURE.md` port table + mermaid rows.

# Constraints
No behavior change; `-pgid`/wire semantics untouched; no globals; no aliases. Btrfs suites fail closed off-Btrfs as today. No RESULT.md.

# Acceptance
- `! grep -rn "slipfork_engine" crates/slipfork-edge/`
- `! grep -n "slipfork-engine" crates/slipfork-edge/Cargo.toml`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-world -p slipfork-edge`
- `cargo test -p slipfork-engine` (report fail-closed Btrfs legs)
