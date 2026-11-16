---
id: M3.8
title: "Rewrite docs/design/world-base-image.md on unified Btrfs subvolumes"
kind: task
status: ready
owner: designer
priority: 2
depends_on: [M3.7]
paths: ["docs/design/world-base-image.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T16:58"
---

# Goal

Rewrite `docs/design/world-base-image.md` so the entire `L0` rootfs + backing-service design rests on unified Btrfs subvolumes and `crun`, with zero `FICLONE`, zero storage-pool, and zero second-engine text.

# Context

- `ADR-0010`/`ARCHITECTURE.md` (M3.7) are the normative source; this design doc carries the operational detail: cache paths, state-subdir layout, mount table, probe transcript, and node sequencing table.
- Today the file still describes `ioctl(FICLONE)` state cloning, `IncusEngine` snapshots, and an `XFS reflink=1` host requirement.

# Design / Invariants

1. Replace every `FICLONE`/reflink/Incus sentence with the single `BtrfsEngine` story: one subvolume per world (rootfs tree + `state/{pgdata,redis,s3}`), RO snapshot via `BTRFS_IOC_SNAP_CREATE_V2` + `BTRFS_SUBVOL_RDONLY`, writable clone via `BTRFS_IOC_SNAP_CREATE_V2`, destroy via `BTRFS_IOC_SUBVOL_SETFLAGS` (clear RO) + `BTRFS_IOC_SNAP_DESTROY_V2`.
2. Require a Btrfs mount (`-o user_subvol_rm_allowed,noatime`) and state the fail-closed `statfs` magic check (`0x9123683e`) that runs before any world directory exists.
3. Keep the `ADR-0009` three-layer execution, `crun` namespace/identity rules, `$UID:$GID` rule, 6-rule image contract, `RootfsDigest`, and `--host-caches` ABI probe intact — change only the storage/snapshot substrate.
4. Keep the measured probe transcript section, re-labelled for the Btrfs host; do not invent measurements (refer to `docs/research/spike-option-a-btrfs-crun.md` for spike numbers).
5. Keep the node-sequencing table at the end consistent with the actual `M3.7`–`M3.19` plan ids.

# Acceptance

```bash
! grep -rniE 'ficlone|incus|zfs|reflink' docs/design/world-base-image.md
grep -n 'BTRFS_IOC_SNAP_CREATE_V2' docs/design/world-base-image.md
grep -n 'user_subvol_rm_allowed,noatime' docs/design/world-base-image.md
grep -n '0x9123683e' docs/design/world-base-image.md
make docs-check
```

Expected: the inverted `grep` prints nothing; the three `grep`s find the ioctl, the exact mount option string, and the magic constant; `make docs-check` exits 0.
