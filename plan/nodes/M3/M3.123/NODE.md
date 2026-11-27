---
id: M3.123
title: "fork output: approved palette and one plan total (no [1/3] -> [3/5] switch)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/daemon/lifecycle/plan.rs
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/tests/critical_fork_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T18:51"
updated: "2026-11-24T19:01"
---

# Problem

Owner's `slipfork fork` screenshot:

```
✓ [1/3] fork   snapshot       34ms
✓ [2/3] git    branch salut   3ms
✓ [3/5] image  api            cached  7ms
• [4/5] setup  uv sync        up to date <1ms
✓ [5/5] start  web            @uv run blastit dev --port 8000 204ms
✓ salut (298ms) -> http://salut--test1.localhost
```

- Keywords (`fork`, `git`, `image`, `setup`, `start`) are **not** magenta; they use the old dim role. `up` has the approved palette, `fork` does not.
- The counter changes denominator mid-stream: `[1/3]`, `[2/3]` (two fork-prep rows) then `[3/5]`…`[5/5]` (world rows). One plan, one total.

# Contract

- `fork` uses the same role map as `up` (keyword magenta, target bold, command plain, timings dim, `│`/`┃`, `✗`/`✓`, `next:` cyan), `NO_COLOR`/`--json` purity.
- One consistent plan total for the fork: prep rows + world rows counted once (e.g. 5), or prep as a distinct nested level; no denominator change mid-stream.
- `--json` plan frames carry the same total/indices.
- PTY capture of `fork` with a fresh image (not cached) and a cached one; `NO_COLOR` capture.

# Acceptance

- Live `fork` on the VM shows magenta keywords and a single denominator; `make check`, `critical_fork_live`; commit `[M3.123] ...`, done, no push.
