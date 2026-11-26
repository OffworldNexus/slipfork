---
id: M3.168
title: "No Rust toolchain pin: clippy passes locally but fails on rustc 1.94 (drop_non_drop, nonminimal_bool)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "rust-toolchain.toml"
  - "crates/slipfork-world/src/snapshot/store/reconcile_serial.rs"
  - crates/slipfork/src/ui/terminal.rs
  - "crates/slipfork/src/daemon/**"
  - Cargo.toml
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:50"
updated: "2026-11-25T13:26"
---

# Problem

There is no `rust-toolchain.toml`. Local `cargo clippy --workspace --all-targets -D warnings` is green, but on the VM (rustc 1.94) the same command flags `drop_non_drop` in `crates/slipfork-world/src/snapshot/store/reconcile_serial.rs` and `nonminimal_bool` in `crates/slipfork/src/ui/terminal.rs`. The branch therefore fails clippy on newer toolchains — a reproducibility hole, not a code-style taste.

# Fix

- Add `rust-toolchain.toml` pinning the channel the repo is developed and released with (match what `cargo --version` reports on the dev host), plus `components = ["rustfmt", "clippy"]`; keep `profile = "minimal"`.
- Fix the two 1.94 lints properly (no `#[allow]` unless the lint is wrong): `drop_non_drop` in `reconcile_serial` (drop the value explicitly or bind it), `nonminimal_bool` in `terminal.rs` (simplify the boolean expression) — so newer toolchains pass even if the pin lags.
- Note in `docs/install.sh`/README only if the build instructions mention a toolchain.

# Acceptance

- `rustup show` respects the pin; `cargo clippy --workspace --all-targets -- -D warnings` green on the pinned toolchain, and the two lints also pass under 1.94 (run it once on the VM).
- `make check`; Triad-light (`review-arch` only); commit `[M3.168] ...`, done, no push.
