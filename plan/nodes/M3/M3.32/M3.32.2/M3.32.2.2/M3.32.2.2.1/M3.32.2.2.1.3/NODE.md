---
id: M3.32.2.2.1.3
title: "slipfork-engine: retire pivot_root init & WorldProcess verbs, migrate protected pod-exec suites"
kind: epic
status: done
priority: 3
depends_on: [M3.32.2.2.1.2]
paths: ["crates/slipfork-engine/**", "crates/slipfork-world/**", Makefile, Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:26"
updated: "2026-11-19T22:58"
---

# Goal

Delete the single merged-root model and move every in-world operation to per-service pod exec.

# Context

Split from `M3.32.2.2.1`. Dropping overlay/`pivot_root` removes the world `/`, so `WorldProcess`-based verbs can no longer resolve `PATH`/`/proc/1/ns/pid`/`pwd`.

- Rewrite `world_init.rs` + `bin/slipfork-world-init*`: no overlay, no `pivot_root`; publish `namespaces.json` with the `user`/`net`/`ipc` paths.
- Delete `exec_in_world`, `entry_command`, `ServiceRuntime::WorldProcess`; replace with `exec_in_service` / `service_entry_command` over `crun exec` in the named service bundle.
- Migrate the affected protected suites (`crates/slipfork-engine/tests/critical_*.rs` and any `crates/slipfork-world` ones) to pod `crun exec`, re-basing real OCI fixtures (`sh`, `stat`, `ipcmk`) rather than mocking. This node is explicitly authorized to edit those protected files; never weaken an assertion or delete a journey.
- Leave `crates/slipfork` daemon boot to `.2.2.3`.

# Acceptance

- `cargo test -p slipfork-engine -p slipfork-world` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- No `pivot_root`, `exec_in_world`, `entry_command`, or `WorldProcess` left under `crates/`.
- Write NO `RESULT.md`.
