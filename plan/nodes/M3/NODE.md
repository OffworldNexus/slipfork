---
id: M3
title: "Option A Btrfs-Only WorldEngine (ADR-0010) & Polyglot Parallel Snapshot-DAG Test Orchestrator (0.3.0)"
kind: milestone
status: ready
priority: 3
depends_on: [M2]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-16T17:25"
---

# Goal

Deliver Slipfork `0.3.0` on the **single** Option A world engine (`BtrfsEngine`, `ADR-0010`) atop `0.2.0`'s `slipforkd` Composition Root and `ADR-0009` `Dockerfile` `L0` rootfs + `crun` service containers:

1. **Option A scorched-earth wipe (Phase 1):** one unified Btrfs subvolume per world (rootfs + `/var/lib/slipfork/state/{pgdata,redis,s3}`), RO snapshots and writable clones through `BTRFS_IOC_*`, fail-closed `statfs` magic probe (`0x9123683e`), `--ipc` world isolation, AppArmor `userns` unit rendering — and complete deletion of every other engine, storage pool, `FICLONE` helper, and their docs, tests, and harness code, with a Btrfs DO-VM harness (`tools/do_vm.py`) proving the real kernel path.
2. **Deterministic `make check` (`M3.1`):** disk-backed `TMPDIR`, bounded `CARGO_BUILD_JOBS`, and serialized world-booting suites so plain `make check` exits 0 under shared-host load without `RUST_TEST_THREADS=1`.
3. **Polyglot parallel test orchestrator (`M3.2`):** `crates/slipfork-test` (`TestCoordinator`, `RunnerProtocol v1`, rolling single-use warm Btrfs world pool, typed `CoverageDriver`) wired into `slipforkd`, `[artifacts.*]`/`[fixtures.*]` materialization (`per_world` never cached), `slipfork test` CLI, `pytest-slipfork` (Adapter #1, warm Zygote) + `CommandTestAdapter` verified by the shared conformance suite, plus `slipfork prune` snapshot/cache GC. (`WeavedRegistrar`, gated on external `H-006`, lives in `M4.1` so `M3` `0.3.0` gates cleanly.)

# Acceptance

- [ ] Every `M3` roadmap exit criterion in `plan/ROADMAP.md` is verified: plain `make check` (zero environment overrides) and `make docs-check` exit 0 on a non-Btrfs dev host; the real `BtrfsEngine` + `crun` + `postgres:18` suites and a bounded `make bench --check` exit 0 on the Btrfs DO VM; `slipfork test -j N` runs real multi-slot parallel suites with typed coverage and surviving `SLIPFORK_OUTPUT_DIR`.
- [ ] `uv run --no-project tools/plan.py validate` exits 0 and no active node carries a path outside its declared globs.
