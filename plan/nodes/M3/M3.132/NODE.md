---
id: M3.132
title: "Fork/drop state UX defects: per-frame counters in drop, ambiguous nested fork names, stale and half-created fork entries"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.120]
paths:
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/view.rs
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/critical_status_live.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T22:02"
updated: "2026-11-25T00:05"
---

# Owner reports (live `~/slproj/test1`, 0.1.1 branch build)

1. `slipfork drop salut` prints four frames with **per-frame totals**: `→ [1/1] fork stopping salut`, `✓ [2/2] fork stopped fork`, `→ [3/3] fork deleting snapshots`, `✓ [4/4] fork deleted snapshots`. M3.123 fixed exactly this for `fork`; `drop` still numbers each frame against itself. One plan, `[1/4]`..`[4/4]`, one final `✓ Dropped <name> (ms)`.
2. **Names are ambiguous across nested forks.** Forks of forks are listed under nested paths (`~/slproj/test1/.forks/bonjour/.forks/salut`). `drop salut` silently picked one (the depth-1 one) and the second `drop salut -f` then said `no fork named salut exists`, while depth-2 `salut` entries remained in `ps`. `drop`/`fork`/`up` must either refuse an ambiguous bare name with one `next:` that shows the qualified path, or accept a qualified form; never pick one by accident.
3. **Ghost/half states survive.** `ps` lists `tralala` with `! checkout missing — run slipfork drop tralala` while its service still shows `running`; the stale world subvolume from the earlier mid-snapshot failure was never reconciled.
4. `ps` mixes running depth-1 forks, stopped depth-2 forks and broken entries in one list with different path depths.

# Fix (this node)

- `drop`: one plan total across stop + delete (shared `plan::PrepFrame` path, same as M3.123), and one verdict row.
- Name resolution: one owner for "which fork does this name mean" — exact qualified match first, bare name only when unique, ambiguity → typed error (`fork-ambiguous`) with the qualified candidates and one copy-pasteable `next:`.
- `ps`: a half-created fork (checkout gone, world/pods behind) is reported as broken with exactly one remedy, and the remedy must converge it (drop everything), not just print.
- Tests: `critical_fork_live` second-fork/nested-name/ambiguity cases; `critical_status_live`/`ps` output for a half-created fork.

# Acceptance

- Live on a Btrfs VM private tree (never `do_vm.py sync`, never `~/demo`): fork-of-fork, bare-name `drop` ambiguity refusal + qualified drop, `drop` single plan total, half-created fork reported and converged to zero leftovers (`ps` clean, world subvolume gone, pods dead).
- `make check`; Triad; commit `[M3.132] ...`, done, no push.
