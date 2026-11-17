---
id: M3
title: "Btrfs-Only WorldEngine (ADR-0010) — Slipfork 0.1.0 (STP v1 parked to 0.2.0)"
kind: milestone
status: active
priority: 3
depends_on: [M2]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-17T15:05"
---

# Goal

Deliver Slipfork `0.1.0` on the **single** Btrfs world engine (`BtrfsEngine`, `ADR-0010`) atop `0.2.0`'s `slipforkd` Composition Root and `ADR-0009` `Dockerfile` `L0` rootfs + `crun` service containers:

1. **Btrfs-only scorched-earth wipe (Phase 1):** one unified Btrfs subvolume per world (rootfs + `/var/lib/slipfork/state/{pgdata,redis,s3}`), RO snapshots and writable clones through `BTRFS_IOC_*`, fail-closed `statfs` magic probe (`0x9123683e`), `--ipc` world isolation, AppArmor `userns` unit rendering — and complete deletion of every other engine, storage pool, `FICLONE` helper, and their docs, tests, and harness code, with a Btrfs DO-VM harness (`tools/do_vm.py`) proving the real kernel path.
2. **Deterministic `make check` (`M3.1`):** disk-backed `TMPDIR`, bounded `CARGO_BUILD_JOBS`, and serialized world-booting suites so plain `make check` exits 0 under shared-host load without `RUST_TEST_THREADS=1`.
3. **Polyglot parallel test orchestrator (`M3.2`, parked for `0.2.0`):** `crates/slipfork-test` (`TestCoordinator`, `RunnerProtocol v1`, rolling single-use warm Btrfs world pool, typed `CoverageDriver`) wired into `slipforkd`, `[artifacts.*]`/`[fixtures.*]` materialization (`per_world` never cached), `slipfork test` CLI, `pytest-slipfork` (Adapter #1, warm Zygote) + `CommandTestAdapter` verified by the shared conformance suite, plus `slipfork prune` snapshot/cache GC. (`WeavedRegistrar`, gated on external `H-006`, lives in `M4.1` so `M3` `0.3.0` gates cleanly.)

# Acceptance

> **OWNER REGROUP RULING (2026-11-16, binding):** the shipped version of this subtree is **`0.1.0`** on `release/0.1.0` (nothing has merged to `develop` yet). `M3.2` (`STP v1` testing protocol) is **parked for `0.2.0`**; do not activate `M3.2` or any `M3.2.*` child. Every node that touches `Cargo.toml`, `packages/opencode-slipfork/package.json`, `CHANGELOG.md`, `zensical.toml`, or `docs/` writes the shipped version as **`0.1.0`**, never `0.3.0`.

- [ ] Every `0.1.0` exit criterion is verified: plain `make check` (zero environment overrides) and `make docs-check` exit 0 on a non-Btrfs dev host; the real `BtrfsEngine` + `crun` + `postgres:18` suites and a bounded `make bench --check` exit 0 on the Btrfs DO VM with `kernel.apparmor_restrict_unprivileged_userns=1` (the `M3.19.1` attachment set, not a sysctl workaround); the `/home/slipfork/demo` project boots, curls, forks, logs, and drops hands-on.
- [ ] `slipfork test` / STP v1 multi-slot execution is **explicitly parked to `0.2.0`** (`M3.2`), not claimed as a `0.1.0` criterion.
- [ ] `uv run --no-project tools/plan.py validate` exits 0 and no active node carries a path outside its declared globs.
