---
id: M3.59
title: "Fix write_bundle home chown on non-1000 unprivileged UIDs & pod_control_lab.rs:91"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-world/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T23:44"
updated: "2026-11-22T23:44"
---

# Goal

`Docs` and all 26/26 `Packages` jobs passed 100% green on PR #2, while `pod_control_lab::a_pod_bundle_keeps_the_world_network_and_never_leaks_secrets` (`crates/slipfork-world/tests/pod_control_lab.rs:91`) failed on GitHub Actions (`uid = 1001`) with:
`write the OCI bundle: Io { action: "chown", path: ".../rootfs/home/slipfork", source: Os { code: 1, kind: PermissionDenied } }`

# Root Cause & Fix

1. `pod_control_lab.rs` constructed a synthetic `PodHost` with hardcoded `uid = 1000, gid = 1000`, while on GitHub Actions `runner` has `euid = 1001, egid = 118` — and an unprivileged Linux process (`geteuid() != 0`) cannot `chown(2)` a directory it created (`1001:118`) to `1000:1000` (`EPERM`)!
2. In `crates/slipfork-world/src/pod.rs` (`write_bundle`):
   - Only call `chown` when the directory's current `(metadata.uid(), metadata.gid()) != (host.uid, host.gid)` (or when `geteuid() == 0`), so unprivileged `$UID:$GID` callers creating `rootfs/home/slipfork` never invoke a redundant `chown`!
   - And in `crates/slipfork-world/tests/pod_control_lab.rs`, pass the real calling process `geteuid()` / `getegid()` (not hardcoded `1000`) to `PodHost::new(...)`!

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p slipfork-world` exits `0`.
