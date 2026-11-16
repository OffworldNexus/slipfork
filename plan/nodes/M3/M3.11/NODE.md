---
id: M3.11
title: Record Spike A measurements (Btrfs + crun) in docs/research and LESSONS
kind: task
status: ready
owner: researcher
priority: 2
depends_on: [M3.7]
paths: ["docs/research/spike-option-a-btrfs-crun.md", docs/knowledge/LESSONS.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T16:58"
---

# Goal

Record Spike A's measured Option A numbers in a new `docs/research/spike-option-a-btrfs-crun.md` (the primary-source evidence for `ADR-0010`) and append the two hard-won Btrfs/IPC facts to `docs/knowledge/LESSONS.md`.

# Context

- `docs/research/` already carries `spike-option-b-kvm-microvm.md` (measured, and now out of scope) and `m1-poc-benchmark.md` (historical XFS/`netns` baseline). Option A has no report yet, so `ADR-0010` would cite nothing.
- Owner-supplied measurements to record: `19.5 ms` read-only snapshot of a world subvolume, `11–24 ms` writable clone, `39 ms` to a warm-SQL world, `6.3 MiB` RAM per world; `--ipc` (`CLONE_NEWIPC`) required so parallel Postgres instances never collide on SysV `shmget` key `90000`; `BTRFS_SUBVOL_RDONLY` must be cleared (`BTRFS_IOC_SUBVOL_SETFLAGS`) before `BTRFS_IOC_SNAP_DESTROY_V2`.

# Design / Invariants

1. Structure the report exactly like the existing spike reports: host + versions header, Question, Results table with one row per measurement and its unit, Method (exact commands/ioctls), What this means for Slipfork, Traps, Unverified. Every number carries its unit and the metric it measures.
2. **Only** the owner-supplied measurements and facts go in; invent nothing, add no `[derived]`/`[guessed]` markers (`tools/guard.py` rejects them), and state plainly that Option B's microVM numbers remain in its own spike file and are no longer part of the design of record.
3. `LESSONS.md`: append exactly two concise bullets — (a) unified-subvolume snapshot/destroy flag ordering, (b) `CLONE_NEWIPC` (`--ipc`) required for parallel Postgres in separate world IPC namespaces. Do not delete existing lessons; the XFS/reflink lesson stays as a host fact.
4. Files: only `docs/research/spike-option-a-btrfs-crun.md` (new) and `docs/knowledge/LESSONS.md`.

# Acceptance

```bash
test -f docs/research/spike-option-a-btrfs-crun.md
grep -n '19.5' docs/research/spike-option-a-btrfs-crun.md
grep -n '11' docs/research/spike-option-a-btrfs-crun.md | grep -c 'ms'
grep -n '6.3 MiB' docs/research/spike-option-a-btrfs-crun.md
grep -n 'CLONE_NEWIPC' docs/research/spike-option-a-btrfs-crun.md
grep -n 'BTRFS_SUBVOL_RDONLY' docs/research/spike-option-a-btrfs-crun.md
grep -n 'CLONE_NEWIPC' docs/knowledge/LESSONS.md
! grep -rn '\[derived\]\|\[guessed\]' docs/research/spike-option-a-btrfs-crun.md docs/knowledge/LESSONS.md
uv run --no-project tools/guard.py
```

Expected: the report exists and contains `19.5`, at least one `… ms` line for the `11–24 ms` clone, `6.3 MiB`, `CLONE_NEWIPC`, `BTRFS_SUBVOL_RDONLY`; `LESSONS.md` contains `CLONE_NEWIPC`; the inverted marker `grep` prints nothing; `guard.py` exits 0.
