---
id: M3.32.2.2.1.3.2.2
title: Retire WorldProcess verbs and rebase protected journeys onto explicit service pods
kind: epic
status: active
priority: 3
depends_on: [M3.32.2.2.1.3.2.1]
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-testkit/**"
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:50"
updated: "2026-11-19T18:43"
---

# Goal

Delete the merged-root vocabulary and rewrite the protected journeys against explicit `ServiceRuntime::ServicePod` + `exec_in_service`.

# Context

Split from `M3.32.2.2.1.3.2`. With the init flip VM-proven, the legacy verbs can go.

- Delete `WorldProcess`, `exec_in_world`, `entry_command`, and leftovers (`run_in_world`, `entry_prefix`).
- Rewrite the ~40 affected call sites in `crates/slipfork-engine/tests/critical_*.rs`, `crates/slipfork-world` tests, `support/mod.rs`, and `slipfork-testkit/src/conformance.rs` to explicit pod services: cwd, mounts, netns inode, retained logs.
- Authorized to edit protected files; never weaken, skip, or delete a journey.
- VM-verify and record suite counts in `LOG.md`.

# Acceptance

- `cargo test -p slipfork-engine -p slipfork-world -p slipfork-testkit` exits 0; no `WorldProcess`/`exec_in_world`/`entry_command`/`run_in_world` under `crates/`.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- `tools/do_vm.py sync && tools/do_vm.py test` exit 0 on `165.232.86.174`.
- Write NO `RESULT.md`.
