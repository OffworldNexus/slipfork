---
id: M3.18
title: "Published docs align with the shipped Btrfs-only surfaces (README, index, getting-started, CLI reference, AGENTS.md)"
kind: task
status: ready
owner: "doc-writer"
priority: 4
depends_on: [M3.17]
paths:
  - README.md
  - docs/index.md
  - "docs/getting-started.md"
  - docs/reference/cli.md
  - AGENTS.md
  - "docs/testing-dag.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T21:01"
---

# Goal

Bring every **published** page in line with the shipped Btrfs-only reality: `README.md`, `docs/index.md`, `docs/getting-started.md`, `docs/reference/cli.md`, and the `AGENTS.md` orientation table describe `slipfork doctor`'s Btrfs probe, the Btrfs host prerequisite, and the `crun` world stack — with zero Incus/ZFS/`FICLONE` claims and zero vaporware.

# Context

- Those files currently tell users `doctor` probes an Incus socket and a 64 KiB `FICLONE` round-trip, and that XFS `reflink=1` or ZFS is an acceptable CoW filesystem. After `M3.15`/`M3.17` those statements are false, and `check_docs_honesty.py`/`docs-check` must catch that.
- Only describe what `M3.15` (doctor/daemon unit) and `M3.17` (support matrix) actually ship; `slipfork test` docs belong to the Phase 2 docs node.

# Design / Invariants

1. `README.md` and `docs/index.md`: prerequisite becomes a Btrfs filesystem mounted with `user_subvol_rm_allowed,noatime` under the state dir (one-line `mkfs.btrfs`/`mount` hint), plus Linux, Docker with buildx, `unshare`/`ip`, and `crun`; the `doctor` example output shows the real Btrfs probe fields; delete "live `IncusEngine`" from the leftovers list.
2. `docs/getting-started.md` and `docs/reference/cli.md`: the doctor transcript matches the shipped JSON/text fields exactly (copy real output, invent nothing); document `slipfork daemon unit --only apparmor` and the Ubuntu 24.04 `userns` restriction; replace the reflink host requirement with the Btrfs requirement and the fail-closed message.
3. `AGENTS.md`: the "Where things are" table and world-engine vocabulary name the Btrfs engine and Btrfs subvolume snapshots; keep every rule and law text unchanged (factual nouns only).
4. Zero vaporware: no command, flag, or field that does not run in this tree; `make docs-check` (Zensical `--strict` plus `check_docs_honesty.py`) is the gate.

# Acceptance

```bash
! grep -rniE 'incus|zfs|ficlone|reflink' README.md docs/index.md docs/getting-started.md docs/reference/cli.md AGENTS.md
grep -n 'user_subvol_rm_allowed,noatime' README.md docs/getting-started.md docs/reference/cli.md
grep -n 'btrfs' README.md docs/index.md docs/getting-started.md docs/reference/cli.md
grep -n -- '--only apparmor' docs/reference/cli.md
make docs-check
```

Expected: the inverted `grep` prints nothing; the two `grep`s find the mount options and `btrfs` in every published page; `--only apparmor` is documented; `make docs-check` exits 0 with zero honesty violations.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
