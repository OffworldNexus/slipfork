---
id: M1.3
title: "Implement core IoC crates (slipfork-snow, slipfork-dag, slipfork-engine, slipfork) & @slipfork/opencode"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M1.2]
paths: [Cargo.toml, Cargo.lock, "crates/**", "packages/opencode-slipfork/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-14T22:30"
---

# Goal

Implement core IoC crates (slipfork-snow, slipfork-dag, slipfork-engine, slipfork) & @slipfork/opencode

# Context

See `ARCHITECTURE.md` and `docs/design/m1-core-and-poc.md`.

# Acceptance

- [ ] `cargo test --workspace --all-targets` exits 0.
