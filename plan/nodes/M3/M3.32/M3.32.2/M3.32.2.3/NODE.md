---
id: M3.32.2.3
title: "slipfork-testkit per-service pod harness & Btrfs live-suite migration"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.2.2.3]
paths:
  - "crates/slipfork-testkit/**"
  - "crates/slipfork-ospp-testkit/**"
  - "crates/slipfork/src/daemon/tests/**"
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:15"
updated: "2026-11-19T13:24"
---

# Goal

Wire the per-service pod engine into `slipforkd` boot and the lab harness, removing the `slipfork.toml` loader.

# Context

See `plan/nodes/M3/M3.32/M3.32.2/NODE.md` lines 31-33 and Acceptance.

- Rewrite `crates/slipfork/src/daemon/{world.rs,boot.rs,backing.rs,base.rs}` into cohesive `PodBoot`/`ServicePod` structs (<= 500 LOC each; split rather than grow `world.rs`), wired only from the composition root (`main.rs`).
- Per-service Btrfs `volume` submounts over the live `MS_BIND` host worktree keeping inotify HMR; CoW volume clone on `fork` stays ~20 ms.
- Drop the `slipfork.toml` loader from `project.rs`/`resolve.rs`; project identity from `Forkfile` `project <slug>` or VCS.
- Update `crates/slipfork/src/daemon/tests/*` for per-service pods; extend `crates/slipfork-testkit/**` where the harness assumed one rootfs.
- Do not delete `slipfork-snow` (that is `M3.32.3`).

# Acceptance

- `cargo test -p slipfork -p slipfork-testkit` exits 0, world-boot tests updated for per-service pods.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- Plan-wide check: no `slipfork.toml` string remains under `crates/`.
- Write NO `RESULT.md`.
