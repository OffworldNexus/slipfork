---
id: M3.9
title: "Rewrite docs/design/target-dx.md for the Btrfs-only world (doctor, world entry)"
kind: task
status: review
owner: designer
priority: 2
depends_on: [M3.7, M3.25]
paths: ["docs/design/target-dx.md"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T21:00"
---

# Goal

Rewrite `docs/design/target-dx.md` (the internal target DX spec) so every command, `doctor` field, world-entry argv, and `slipfork test` example reflects the Btrfs-only Option A world — with zero `incus exec` entries and zero reflink/`IncusEngine` wording.

# Context

- `target-dx.md` is the *dream-tool* DX contract that published pages are promoted from (`dx-first`), so stale engine nouns here leak into `docs/` and `README.md` later.
- `M3.7` fixes `ADR-0010`/`ARCHITECTURE.md`; this node aligns the DX examples with those contracts (`WorldEntryCommand` = `nsenter` into the world's `slipfork-world-init`, service entry = `crun exec`).

# Design / Invariants

1. Every world-entry example names `nsenter --target <init_pid>` into the Btrfs world's PID 1 (and `crun exec` for `slipfork db`/`redis`); delete every alternative engine entry.
2. `slipfork doctor` examples show the Btrfs probe (`filesystem: btrfs`, `BTRFS_SUPER_MAGIC` match, subvolume ioctl probe, `netns`, `crun`) and the exact failure text for a non-Btrfs state dir naming `mount -o user_subvol_rm_allowed,noatime`.
3. Keep the `M3` section (`slipfork test` command row, `[test]` coverage example, `SLIPFORK_OUTPUT_DIR`) — it is still the target DX, with world wording changed from "CoW reflink worlds" to "Btrfs subvolume worlds".
4. Keep every `--json` schema and the `--world <fork>` selector rules byte-for-byte (they are shipped contracts).
5. No new vaporware: only describe surfaces that `0.2.0` shipped or that `M3` nodes in this plan deliver.

# Acceptance

```bash
! grep -rniE 'incus|reflink|zfs|ficlone' docs/design/target-dx.md
grep -n 'nsenter --target' docs/design/target-dx.md
grep -n 'crun exec' docs/design/target-dx.md
grep -n 'slipfork test' docs/design/target-dx.md
grep -n 'user_subvol_rm_allowed,noatime' docs/design/target-dx.md
make docs-check
```

Expected: the inverted `grep` prints nothing; the four `grep`s find world entry, service entry, the `slipfork test` row, and the mount-option failure text; `make docs-check` exits 0.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
