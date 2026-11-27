---
id: M3.133
title: "drop: one plan total and consistent rows (per-frame totals 1/1..4/4)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/src/daemon/lifecycle/plan.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/tests/critical_fork_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T22:06"
updated: "2026-11-24T23:38"
---

# Problem

`slipfork drop <name>` still numbers every frame against itself:

```
→ [1/1] fork stopping jello
✓ [2/2] fork  stopped fork    done  pods stopped  100ms
→ [3/3] fork deleting snapshots
✓ [4/4] fork  deleted snapshots   done  project + world data  101ms
✓ Dropped jello (101ms)
```

M3.123 fixed this exact defect for `fork` (one merged plan total, shared outer-row roles); `drop` was missed. The labels are also inconsistent (`stopped fork` as a target, `fork deleting snapshots` vs `deleted snapshots`, uneven padding).

# Fix

- One plan of four steps with `[i/4]` in every frame: stop (started/finished), delete snapshots (started/finished), then a single `✓ Dropped <name> (ms)` verdict. Use the same `plan::PrepFrame`/outer-row roles as `fork`; no local counter arithmetic.
- Consistent rows, e.g. `stop  jello  done  pods stopped` / `delete  jello  done  project + world data`, keyword magenta, state green, timing meta, dynamic widths (never a hardcoded pad).
- `--json` frames carry the plan `index`/`total` from one plan; `NO_COLOR`/`--json` zero escapes.
- Byte-exact test in `critical_fork_live` for the four frames + verdict, plain and coloured.

# Acceptance

- Live on a Btrfs VM private tree (never `do_vm.py sync`, never `~/demo`): the drop capture shows `[1/4]`..`[4/4]` + one `Dropped` verdict; `--json` totals match.
- `make check`; Triad; commit `[M3.133] ...`, done, no push.
