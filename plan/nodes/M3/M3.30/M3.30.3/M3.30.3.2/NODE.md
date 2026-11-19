---
id: M3.30.3.2
title: "Live streamed progress on rootfs buildx/pull/export, daemon wait and install legs"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.3.1]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**", "crates/slipfork-engine/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T01:25"
updated: "2026-11-19T01:25"
---

# Goal
Section 2 of `plan/nodes/M3/M3.30/M3.30.3/NODE.md`.

# Work
- `BootEvent::Progress { step, total, phase, detail }` rendered with `[step/total]` headers + elapsed time via TerminalUi on stderr; silent under --json/non-TTY.
- `crates/slipfork-engine/src/rootfs/buildx.rs`: spawn + line-streamed phase progress instead of blocking output().
- `crates/slipfork-engine/src/rootfs/oci_store.rs`: docker pull/export live lines (drop -q and output()).
- daemon-wait silent sleep loop: indicatif spinner with elapsed time and a next: remedy on timeout.
- `slipfork daemon install` systemctl/apparmor legs: phase lines + spawn (carry-over).

# Constraints
--json stdout stays machine-pure (progress on stderr); no swallowed child output; real subprocesses only. No RESULT.md.

# Acceptance
- no blocking output() or -q in rootfs/oci_store.rs and rootfs/buildx.rs
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test -p slipfork-engine --lib
- cargo test -p slipfork --lib
