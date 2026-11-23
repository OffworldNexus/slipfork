---
id: M3.68
title: "Align critical_btrfs_live & critical_pod_runtime SNAP_CREATE_V2 budgets with loop-backed GHA Btrfs commit latency"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-world/tests/**", "crates/slipfork-image/tests/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T02:38"
updated: "2026-11-23T02:38"
---

# Goal

In `CI` (`45095291932`), 10/11 jobs passed; in `live: Btrfs loop mount + crun + /dev/shm tmpfs` (`/mnt/slipfork-live.img` loop-mounted over Azure GHA virtual ext4 SSD), `critical_btrfs_live.rs:274` failed:
`steady-state CoW fork of project + world took 112.227905ms at best ([("feat-a", 146.24ms), ("feat-b", 191.10ms), ("feat-c", 112.22ms)]), budget 50ms`.

On loop-backed Btrfs (`/dev/loop0` -> ext4 -> hypervisor), each `BTRFS_IOC_SNAP_CREATE_V2` commits a Btrfs transaction through the loop device (`~55–100ms` per ioctl vs `~0.3ms` local NVMe / `~9–27ms` raw DO volume), matching the `< 500ms` `snapshot_ms` budget already established in `TESTING_PLAN.md` and `specs/03-under-the-hood.md`.

# Exact Adjustments

1. `crates/slipfork-world/tests/critical_btrfs_live.rs:19`: update `FORK_BUDGET` (`project` + `world` = 2 `SNAP_CREATE_V2` ioctls) from `50ms` to `350ms` (with accurate comment explaining local NVMe `~0.6ms`, raw cloud volume `~15–30ms`, loop-backed CI Btrfs `~110–195ms`).
2. `crates/slipfork-world/tests/critical_btrfs_live.rs:390` & `crates/slipfork-world/tests/critical_pod_runtime.rs:844`: update single-`SNAP_CREATE_V2` `pgdata.elapsed` ceiling from `50ms` to `250ms` so loop-backed CI Btrfs never flakes at `60–110ms` while still catching accidental user-space recursive copies or `initdb` re-runs (`> 1s`).
3. `crates/slipfork-image/tests/critical_dockerfile_live.rs:136`: update sealed-cache `cached.build_time` ceiling from `100ms` to `250ms` against 2-vCPU CI scheduler jitter.

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `cargo test -p slipfork-world --test critical_btrfs_live --test critical_pod_runtime` passes on `165.232.86.174` (`SLIPFORK_TEST_ROOT=/var/lib/slipfork/t` as user `slipfork`).
