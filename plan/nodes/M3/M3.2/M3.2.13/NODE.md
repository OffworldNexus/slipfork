---
id: M3.2.13
title: "Snapshot + rootfs/OCI cache GC in slipfork prune (dry-run, live-world safe, idempotent)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.2.12]
paths:
  - "crates/slipfork-engine/src/prune.rs"
  - "crates/slipfork-engine/src/lib.rs"
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/tests/critical_cli.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Add snapshot and cache garbage collection to `slipfork prune`: destroy unreferenced RO world snapshots (subvolumes), unreferenced `<state_dir>/cache/rootfs/<digest>/app` trees, and unreferenced OCI rootfs entries — typed, dry-run capable, and never touching a live world or the active `DigestCache` lineage.

# Context

- `M3`'s roadmap exit criterion 1 pairs `M3.1` (deterministic `make check`) with snapshot + `cache/{rootfs,oci}` GC in `slipfork prune`. Option A makes this concrete: every world and fixture snapshot is one Btrfs subvolume.
- `DaemonControl::prune` already exists with `--dry-run`; today it reports orphaned worlds only.

# Design / Invariants

1. `crates/slipfork-engine/src/prune.rs`: a pure planner over disk state (`list worlds/snapshots/rootfs digests/oci digests` + liveness set supplied by the daemon) producing a typed `PrunePlan { destroy_worlds, destroy_snapshots, destroy_rootfs, destroy_oci, reclaim_bytes }`; execution is a separate step using the existing `SubvolumeDriver`/`destroy_world` path (clear `BTRFS_SUBVOL_RDONLY` before destroy).
2. Liveness is explicit: live worlds, snapshots referenced by `DigestCache`/run trees, and the rootfs digest of any registered world are never collected. Unknown on-disk entries are reported, not deleted.
3. `--dry-run` prints the plan and changes nothing; `--yes` executes; `--json` emits the same plan object. Idempotent: a second run reports zero reclaim.
4. Never traverse outside `<state_dir>`; refuse to act when `<state_dir>` is not the probed Btrfs filesystem.
5. Tests: `crates/slipfork/tests/critical_cli.rs` (real crate paths, temp state dir) asserts dry-run non-mutation, live-world protection, reclaim accounting, idempotency, and the refuse-on-non-Btrfs behaviour via an injected driver; unit tests in `prune.rs` cover the pure planner.

# Acceptance

```bash
cargo test -p slipfork-engine --all-targets
cargo test -p slipfork --all-targets
cargo clippy --workspace --all-targets -- -D warnings
grep -n 'PrunePlan' crates/slipfork-engine/src/prune.rs crates/slipfork/src/daemon.rs
grep -n 'dry_run\|dry-run' crates/slipfork/src/daemon.rs
uv run --no-project tools/plan.py validate
make rs-test
```

Expected: both test invocations and workspace clippy exit 0; `PrunePlan` is wired from engine to daemon; dry-run is honoured; `plan.py validate` and `make rs-test` exit 0.
