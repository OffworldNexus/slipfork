---
id: M3.45.4
title: "Milestone sign-off fixes: doc honesty, pod.rs split, snapshot provenance, dead sweeps, single-impl signer trait"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.45.3]
paths: ["crates/**", "docs/**", "specs/**", "tools/**", TESTING_PLAN.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T03:34"
updated: "2026-11-22T03:34"
---

# Goal
Close the M3 milestone Triad findings: no published false timing claim, every file <= 500 LOC, measured snapshot provenance, and zero single-impl traits / duplicated sweeps.

# Acceptance
- docs/index.md numeric fork claim matches measured reality (~0.8s end-to-end; 0.3-50ms per CoW ioctl).
- crates/slipfork-world/src/bin/world_init/pod.rs split so every .rs <= 500 LOC.
- critical_fork_live asserts snapshot_ms <= 50 * <concurrent ioctl count> with the recorded probe cited.
- One pod sweep + one reusable-pod predicate (engine-owned); delete the pre-RuntimePods dead sweep.
- DeviceKeySigner single-impl trait removed in favor of injecting the concrete signer; DaemonControl port either implemented or the spec amended.
- cargo fmt/clippy/cargo test --workspace green; make check + make docs-check exit 0.
