---
id: M3.30.1.3.1
title: "Engine substrate: concrete clock/driver DI, delete RecordingDriver/TestTreeDriver theater"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-engine/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T22:46"
updated: "2026-11-18T23:04"
---

# Goal
Slice 1 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/NODE.md` (read parent + epic section 4): make the engine substrate concrete and delete the driver/clock doubles.

# Work
- `MonotonicClock` (crates/slipfork/src/clock.rs:29) + `HandoverClock` (crates/slipfork/src/daemon_unit_install.rs:67): replace with plain `Duration` / `StopBudget` values passed by the caller. (daemon_unit_install belongs to slice 3; only remove the clock trait itself and its engine uses here.)
- Collapse `BtrfsEngine` onto the concrete `KernelBtrfsDriver` and delete the `SubvolumeDriver` trait when its only second impl is the `chmod 0o555` `TestTreeDriver`. Delete `RecordingDriver` (btrfs_engine.rs:2603) and the engine `FakeClock` (btrfs_engine.rs:2820).
- Delete `TestTreeDriver` + its unit tests from `crates/slipfork-testkit/src/btrfs.rs` and all testkit wiring that exists only for it.
- Rewire `critical_engine` / `critical_oci_runtime` / `critical_btrfs_live` / engine `tests/support/mod.rs` onto the real driver; keep the live Btrfs leg intact and fail closed with ONE clear line on non-Btrfs hosts (never fake it).

# Constraints
- True Constructor DI of concrete structs; no globals/OnceLock/std::env::var in library crates; no aliases/#[deprecated]. No mocks. No RESULT.md.
- Expected on this host: live Btrfs suites fail closed; report the exact one-line message.

# Acceptance
- `! grep -rEn "TestTreeDriver|RecordingDriver|FakeClock|trait MonotonicClock|trait SubvolumeDriver" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-engine` (live Btrfs legs may fail closed off-Btrfs; report)
- `cargo test -p slipfork --lib`
