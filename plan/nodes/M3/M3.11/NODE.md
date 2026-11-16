---
id: M3.11
title: "Record Btrfs + crun benchmark measurements in docs/research (canonical name), delete the rejected-option report, and scrub Option/Spike naming from LESSONS"
kind: task
status: review
owner: researcher
priority: 2
depends_on: [M3.7]
paths:
  - "docs/research/btrfs-crun-benchmarks.md"
  - "docs/research/spike-option-b-kvm-microvm.md"
  - docs/knowledge/LESSONS.md
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T21:10"
---

# Goal

Record the owner-supplied Btrfs + `crun` measurements in a new `docs/research/btrfs-crun-benchmarks.md` (the primary-source evidence for `ADR-0010`), delete the obsolete rejected-option report, and append the two hard-won Btrfs/IPC facts to `docs/knowledge/LESSONS.md`.

# Context

- The benchmark report is cited by `ADR-0010` under its canonical name `docs/research/btrfs-crun-benchmarks.md`.
- `docs/research/` still carries `spike-option-b-kvm-microvm.md`, a measured report for an approach Slipfork does not take; the owner requires it deleted so no reader can believe a second architecture exists (`m1-poc-benchmark.md` stays as a labelled historical baseline).
- OWNER TERMINOLOGY RULE (2026-11-16): no `Option A`/`Option B`/`Spike A`/`Spike B` framing anywhere. State the unified Btrfs subvolume + Linux namespaces + `crun` (`BtrfsEngine`) architecture directly as what Slipfork is and does.
- Owner-supplied measurements to record: `19.5 ms` read-only snapshot of a world subvolume, `11–24 ms` writable clone, `39 ms` to a warm-SQL world, `6.3 MiB` RAM per world; `--ipc` (`CLONE_NEWIPC`) required so parallel Postgres instances never collide on SysV `shmget` key `90000`; `BTRFS_SUBVOL_RDONLY` must be cleared (`BTRFS_IOC_SUBVOL_SETFLAGS`) before `BTRFS_IOC_SNAP_DESTROY_V2`.

# Design / Invariants

1. Structure the report like the existing research reports: host + versions header, Question, Results table with one row per measurement and its unit, Method (exact commands/ioctls), What this means for Slipfork, Traps, Unverified. Every number carries its unit and the metric it measures.
2. **Only** the owner-supplied measurements and facts go in; invent nothing and add no `[derived]`/`[guessed]` markers (`tools/guard.py` rejects them). No alternative-architecture framing: the report measures *this* architecture.
3. Delete `docs/research/spike-option-b-kvm-microvm.md` outright (owner ruling). `m1-poc-benchmark.md` stays untouched and keeps its historical-baseline label. `docs/design/**` is out of scope here.
4. `LESSONS.md`: append exactly two concise bullets — (a) unified-subvolume snapshot/destroy flag ordering **scoped to `KernelBtrfsDriver`/`src/btrfs.rs`** (the tree driver cannot ioctl), (b) `CLONE_NEWIPC` (`--ipc`) required for parallel Postgres in separate world IPC namespaces, stating only the `shmget` key `90000` cause (no duplicate `crun` `ipc`-namespace mandate). **Also delete the microVM lessons** (virtiofs snapshot/restore, hypervisor VMM kill timings, microVM API/snapshot-path traps) in this same sweep — `LEAD RULING`: the owner's "forget there was another option" directive supersedes "do not delete existing lessons", so no rejected-architecture guidance survives the deletion of its report. Keep every host fact that is still true (XFS/reflink as host behaviour, `buildx` ownership, `crun` unprivileged quirks). Scrub any `Option A/B` or `Spike A/B` wording.
5. The report is the **sole owner** of the four measurements; do not restate them in more than one table entry plus the prose that explains them, and never as a design rule (point at `ADR-0010` §3/§4/§6 for the normative behaviour).
5. Files: only `docs/research/btrfs-crun-benchmarks.md` (new), `docs/research/spike-option-b-kvm-microvm.md` (deleted), and `docs/knowledge/LESSONS.md`.

# Acceptance

```bash
test -f docs/research/btrfs-crun-benchmarks.md
test ! -e docs/research/spike-option-b-kvm-microvm.md
grep -n '19.5' docs/research/btrfs-crun-benchmarks.md
grep -n '11' docs/research/btrfs-crun-benchmarks.md | grep -c 'ms'
grep -n '6.3 MiB' docs/research/btrfs-crun-benchmarks.md
grep -n 'CLONE_NEWIPC' docs/research/btrfs-crun-benchmarks.md
grep -n 'BTRFS_SUBVOL_RDONLY' docs/research/btrfs-crun-benchmarks.md
grep -n 'CLONE_NEWIPC' docs/knowledge/LESSONS.md
! grep -riE 'option [ab]|spike [ab]' docs/research/btrfs-crun-benchmarks.md docs/knowledge/LESSONS.md
! grep -rn '\[derived\]\|\[guessed\]' docs/research/btrfs-crun-benchmarks.md docs/knowledge/LESSONS.md
uv run --no-project tools/guard.py
```

Expected: the report exists under its canonical name, the obsolete report is gone, the report contains `19.5`, at least one `… ms` line for the `11–24 ms` clone, `6.3 MiB`, `CLONE_NEWIPC`, `BTRFS_SUBVOL_RDONLY`; `LESSONS.md` contains `CLONE_NEWIPC`; both inverted `grep`s print nothing; `guard.py` exits 0.
