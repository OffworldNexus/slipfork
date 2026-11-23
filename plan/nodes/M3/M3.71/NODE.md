---
id: M3.71
title: "Fix critical_dockerfile_live:334 crun runtime dir lookup under XDG_RUNTIME_DIR=/dev/shm/sf-live-runtime"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-image/tests/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T15:50"
updated: "2026-11-23T15:53"
---

# Goal

In `CI` (`45197183705`), 10/11 jobs passed; in `make live-test` (`XDG_RUNTIME_DIR=/dev/shm/sf-live-runtime`), `crates/slipfork-image/tests/critical_dockerfile_live.rs:373` failed:
`step cleanup must remove '/run/user/1001/crun/sf-3041-0123456789abcdef' after completion`.

# Exact Root Cause & Fix

1. `crun` resolves its root directory from `$XDG_RUNTIME_DIR/crun` first, falling back to `/run/user/{uid}/crun` only when `XDG_RUNTIME_DIR` is unset. `make live-test` sets `XDG_RUNTIME_DIR=/dev/shm/sf-live-runtime`, while line 334 hardcoded `PathBuf::from(format!("/run/user/{uid}"))` — so `stale_state` was planted in `/run/user/1001/crun` while `crun` actually operated in `/dev/shm/sf-live-runtime/crun`!
2. In `crates/slipfork-image/tests/critical_dockerfile_live.rs:334`, resolve `runtime_user_dir` from `std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")))` so the test matches `crun`'s exact runtime root both with and without `XDG_RUNTIME_DIR`.

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `XDG_RUNTIME_DIR=/dev/shm/sf-live-runtime SLIPFORK_TEST_ROOT=/var/lib/slipfork/t cargo test -p slipfork-image --test critical_dockerfile_live -- --test-threads=1` passes `3/3` on `165.232.86.174` as user `slipfork`.
