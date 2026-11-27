---
id: M3.182.3
title: Split the daemon fork/client/mod modules
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.182.2]
paths:
  - "crates/slipfork/src/daemon/**"
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/world/emit.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/up/reply.rs
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/runtime/lifecycle.rs
  - crates/slipfork/src/world/runtime/execute.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T21:45"
updated: "2026-11-25T22:12"
---

# Goal

Split the three over-budget daemon modules along their existing seams, keeping RPC framing and fork behaviour byte-identical.

# Context (pre-baked, verified at HEAD 4173bd7)

- `crates/slipfork/src/daemon/lifecycle/fork.rs` = 816 lines (715 production): `impl ForkTarget` 66, `impl Core` 87-661, free helpers 662-717, tests 718. Split cohesive method groups into `daemon/lifecycle/fork/{mod,<seam>}.rs` (copy/params/vcs seams visible from the helper names `project_param`, `param_name`, `btrfs_failure`, `vcs_failure`).
- `crates/slipfork/src/daemon/client.rs` = 606 lines (550 production): `DaemonError` 26-46, `ConnectNotice` 53-83, `DaemonClient` 84-481, `parse_response` 482, `refusal_frame` 525, `is_absent` 531, `startup_remedy` 539, tests 552 -> `daemon/client/{mod,wire,error}.rs`.
- `crates/slipfork/src/daemon/mod.rs` = 523 lines: `Failure` 63, `Startup` 73-118, `socket_path` 92, `run` 97, `Daemon` 140. Move `Daemon` + its serving loop into `daemon/serving.rs`; keep `run`/`socket_path` reachable at the same paths.

# Acceptance

- Production LOC <= 500 per file (`awk '/^#\[cfg\(test\)\]/{f=1} !f' | wc -l`).
- Byte-captured `fork`/`down`/`ps` behaviour unchanged (existing daemon tests + `cargo test -p slipfork`); `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Do **not** touch `crates/slipfork/src/world/**`, `ui/**`, `boot*`, `commands/**`, `doctor/**`, `status/**` (other slices), `specs/`, `TESTING_PLAN.md`, `CHANGELOG.md`, `docs/`, or any `critical_*.rs`.
