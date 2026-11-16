---
id: M3.7
title: "Design of record: ADR-0010 Btrfs-only WorldEngine, STP v1-ready DI tree, and ADR-0001..0009 consolidation"
kind: task
status: done
owner: architect
priority: 1
depends_on: []
paths:
  - "docs/decisions/ADR-0010-btrfs-only-world-engine.md"
  - "docs/decisions/ADR-0001-snow-modes-and-secret-protocol.md"
  - "docs/decisions/ADR-0002-world-engine-ioc-btrfs.md"
  - "docs/decisions/ADR-0003-orthogonal-artifact-and-fixture-dag.md"
  - "docs/decisions/ADR-0004-opencode-implicit-worktree-routing.md"
  - "docs/decisions/ADR-0005-localhost-edge-weaved-ipc-and-vcs-remote.md"
  - "docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md"
  - "docs/decisions/ADR-0007-integration-support-tiers.md"
  - "docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md"
  - "docs/decisions/ADR-0009-world-base-image-and-image-contract.md"
  - ARCHITECTURE.md
  - docs/VISION.md
attempts: 4
max_attempts: 4
escalation: architect
created: "2026-11-16T16:58"
updated: "2026-11-16T20:48"
---

# Goal

Land the **single design of record** for Option A — a Btrfs-only `WorldEngine` — and scorched-earth wipe every non-Option-A concept from `ARCHITECTURE.md`, `docs/VISION.md`, and `ADR-0001`..`ADR-0009`, adding `ADR-0010` as the binding ruling.

# Context

- Owner ruling (2026-11-16, `plan/nodes/M3/LOG.md`): Option A (one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + `crun`) is the **sole** engine. Incus, `incusd`, ZFS, XFS `FICLONE`, QEMU/KVM, curations, and every fallback are deleted with zero backward compatibility.
- Measured basis (`M3.11` records `docs/research/btrfs-crun-benchmarks.md`): `19.5 ms` read-only snapshot, `11–24 ms` writable clone, `39 ms` to warm SQL, `6.3 MiB` RSS per world; `--ipc` (`CLONE_NEWIPC`) is required or parallel Postgres instances collide on SysV `shmget` key `90000`; a read-only subvolume must have `BTRFS_SUBVOL_RDONLY` cleared (`BTRFS_IOC_SUBVOL_SETFLAGS`) before `BTRFS_IOC_SNAP_DESTROY_V2`.
- Design docs that consume this contract (`world-base-image.md`, `target-dx.md`, `do-vm-harness.md`) are separate designer nodes; this node owns only `ARCHITECTURE.md`, `docs/VISION.md`, and `docs/decisions/`.

# Design / Invariants (must be encoded, not paraphrased away)

1. **`ADR-0010-btrfs-only-world-engine.md`** (`Status: Accepted`): records the ruling, the unified-subvolume layout, the ioctl sequence, the fail-closed host probe, the injected driver seam, the `--ipc` requirement, and the AppArmor `userns` requirement. Describe the deleted alternatives generically ("all previously shipped engine adapters, their daemon/REST protocol, their storage pools, and every FICLONE-class helper are deleted") without naming product names anywhere in the design of record.
2. **`ADR-0002` is rewritten in place as the `WorldEngine` IoC contract**: rename the file to `docs/decisions/ADR-0002-world-engine-ioc-btrfs.md`, delete the old `*-incus-and-reflink.md` file, and replace the content with `BtrfsEngine` as the sole production adapter behind `trait WorldEngine`. Update every `ADR-0002` reference inside `ARCHITECTURE.md`/`ADR-0009` accordingly.
3. **`ARCHITECTURE.md` DI tree**: one production `WorldEngine` (`BtrfsEngine`), one injected `SubvolumeDriver` seam with `KernelBtrfsDriver` in production and a crate-local test driver used only by `#[cfg(test)]` / test-crate code; zero `match engine`, zero `EngineCapabilities`, zero `SnapshotMode::StatefulRam` (delete `SnapshotMode` entirely — RO snapshot vs writable clone are distinct methods by signature, not a flag).
4. **Host contract**: `BtrfsEngine` verifies `statfs(<state_dir>).f_type == BTRFS_SUPER_MAGIC (0x9123683e)` at construction/boot and fails closed with `EngineError::BtrfsRequired` naming the exact fix (`mount -o user_subvol_rm_allowed,noatime`). No probe-and-degrade path exists.
5. **World layout**: every world (`<project>-base`, fixtures, `canonical`, `<fork>`, `test-<slot>`) is exactly **one** Btrfs subvolume holding both the `L0` rootfs tree and `/var/lib/slipfork/state/{pgdata,redis,s3}`, so one ioctl snapshots the whole world atomically.
6. **Process isolation**: `slipfork-world-init` is launched with `--user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork --mount-proc`; the `--ipc` flag is normative and explained (SysV IPC per world).
7. **Unprivileged Ubuntu 24.04**: `slipfork daemon unit` also renders the 4-line `/etc/apparmor.d/slipfork` `userns` profile (`--only apparmor`), required with `kernel.apparmor_restrict_unprivileged_userns=1`.
8. `ARCHITECTURE.md` §1 DI tree, §2 crate table, §3.3 `WorldEngine`, §4 host items: `H-004` becomes "closed by owner ruling — Incus path deleted, superseded by `ADR-0010`" (no open item), and `H-006` (`M4.1`) stays gated on `tunnel-weaver`'s IPC spec.

# Acceptance

```bash
test -f docs/decisions/ADR-0010-btrfs-only-world-engine.md
grep -n '0x9123683e\|BTRFS_IOC_SNAP_CREATE_V2\|BTRFS_SUBVOL_RDONLY\|BTRFS_IOC_SUBVOL_SETFLAGS\|BTRFS_IOC_SNAP_DESTROY_V2' docs/decisions/ADR-0010-btrfs-only-world-engine.md
grep -n 'CLONE_NEWIPC' docs/decisions/ADR-0010-btrfs-only-world-engine.md ARCHITECTURE.md
grep -n 'BtrfsEngine\|SubvolumeDriver\|KernelBtrfsDriver\|BtrfsRequired' ARCHITECTURE.md
test -f docs/decisions/ADR-0002-world-engine-ioc-btrfs.md
test ! -e docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md
! grep -rniE 'incus|zfs|ficlone|qemu|kvm|statefulram|reflink' ARCHITECTURE.md docs/VISION.md docs/decisions/
uvx --offline zensical build --strict
uv run --no-project tools/check_docs_honesty.py
uv run --no-project tools/plan.py validate
```

Expected: the ADR exists with all five ioctl/flag names and the `--ipc` requirement; `ARCHITECTURE.md` names `BtrfsEngine`/`SubvolumeDriver`/`KernelBtrfsDriver`/`BtrfsRequired`; the old `ADR-0002` filename is gone; the inverted `grep` prints nothing (exit 0 because of `!`); Zensical `--strict`, `check_docs_honesty.py`, and `plan.py validate` all exit 0.
