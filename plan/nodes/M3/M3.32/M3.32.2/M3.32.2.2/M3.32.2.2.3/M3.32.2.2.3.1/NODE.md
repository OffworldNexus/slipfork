---
id: M3.32.2.2.3.1
title: Explicit Interactive/Exec entry kind in wire + engine; delete INTERACTIVE_ENTRY_SHIM
kind: task
status: done
owner: implementer
priority: 2
depends_on: []
paths:
  - "crates/slipfork-wire/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - crates/slipfork/src/entry.rs
  - crates/slipfork/src/commands/process.rs
  - crates/slipfork/src/daemon/protocol.rs
  - crates/slipfork/tests/critical_l0_wiring.rs
  - crates/slipfork/tests/critical_ospp_daemon.rs
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T19:03"
updated: "2026-11-19T19:48"
---

# Goal

Replace the shared PTY shim with an explicit entry kind so interactive entry needs no `/bin/sh -c`, and expose the pod seam the daemon will use.

# Context

Split from `M3.32.2.2.3`. `INTERACTIVE_ENTRY_SHIM` lives at `crates/slipfork-engine/src/btrfs_engine/mod.rs:380`; `entry_command`/`exec_in_world` are `WorldEngine` methods used by `daemon/{protocol,env}.rs`.

- Add an entry kind (`Interactive` / `Exec`) to the exec target/request in `slipfork-wire` and drive the PTY relay from it; delete `INTERACTIVE_ENTRY_SHIM` and the `/bin/sh -c` it builds.
- Expose the pod seam the daemon needs (`world_pod_namespaces`, `pod_bundle_dir`, pod volume plan); keep `entry_command`/`exec_in_world` as the shrinking bridge for now.
- Update wire docs and any protocol fixtures.

# Acceptance

- No `/bin/sh -c` in non-test engine code.
- `cargo test -p slipfork-wire -p slipfork-engine -p slipfork-world` exits 0; `--all-targets --no-run` links; clippy/fmt clean.
- VM `tools/do_vm.py sync && tools/do_vm.py test` exit 0 (12 suites).
- Write NO `RESULT.md`.
