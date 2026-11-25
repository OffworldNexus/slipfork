---
id: M3.150
title: "Image steps stall silently after a heavy RUN (seal/record tail): measure, surface a phase row, drop the extra commit"
kind: task
status: active
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-image/src/builder/**"
  - "crates/slipfork-image/src/plan/**"
  - "crates/slipfork-world/src/snapshot/**"
  - "crates/slipfork/src/boot/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T09:31"
updated: "2026-11-25T09:43"
---

# Owner report

`sh /tmp/oni.sh && apt-get install -y blastit blastit-python-3.14` finishes in ~3 s, then `up` sits there for much longer with no row moving — "seemingly doing nothing" — before the next step.

# Hypothesis to prove, not assume

After a RUN exits, the builder seals the step: `SETFLAGS(READONLY)` on the step subvolume (a synchronous Btrfs transaction commit over every page apt just dirtied), then record + rename. There is no `syncfs` per step (confirmed: `flush_store` is only used by fork/boot), so the stall is most likely the readonly-flag commit (or the record write), not the child.

# Work

1. **Measure**: timestamp each phase (`exec`, `seal`, `record`, `rename`) into the run log / a `-v` note, so `logs -r` shows where the seconds went. No guessing in the commit message — put the numbers in the node LOG.
2. **Never look hung**: the step's row must show the tail phase while it runs (e.g. `5/7 run … (sealing)` / an elapsed timer that keeps ticking), consistent with `Report`/`Outcome` (no new print paths). A phase longer than ~200 ms must be visible.
3. **Remove the avoidable commit** if the measurement confirms it: create the step node read-only in one ioctl (`SNAP_CREATE_V2` with the RO flag) instead of writing `SETFLAGS` on a dirty writable subvolume, or batch the flags for the image's steps and seal once at image end if per-step sealing is not load-bearing. Keep one owner for "sealed node" and do not regress `is_sealed` semantics or GC.
4. Keep the cache contract: unchanged steps stay `cached`; a changed step rebuilds from the edited instruction.

# Acceptance

- Numbers in the LOG: RUN wall time vs seal/record time for a real `apt-get install` on the VM, before and after.
- Live PTY capture: the row shows a moving elapsed time and the current phase through the tail; the next step starts without a silent gap; total `up` time for the owner's Dockerfile drops materially (state the delta).
- `make check`, Triad, commit `[M3.150] ...`, done, no push.
