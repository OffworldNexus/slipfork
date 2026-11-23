---
id: M3.60
title: "Align critical_world_journey.rs:1511 warm up --json hook assertion with M3.57 warm stamp reuse & verify all 8 live suites"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork/**"
  - "specs/**"
  - "docs/**"
  - README.md
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T23:54"
updated: "2026-11-23T01:20"
---

# Goal

In `CI` run `45079869720`, 10/11 jobs passed (`make check` is now 100% green!), and in `live-test`, `critical_world_journey.rs:1511` failed at the very last assertion of the 18.5s journey:
`the '--json' stream must carry a hook's live output line` (`line.contains("pre-deploy hook ran")`).

# Exact Root Cause & Fix

1. At line ~1480 of `crates/slipfork/tests/critical_world_journey.rs`, `good` had already run `up` -> `down` earlier in the journey, so under `M3.57`'s warm-`up` stamp reuse, `app:pre-deploy` truthfully reported `"state": "up to date"` (`0ms`) without re-running!
2. Update that `--json` boot check in `crates/slipfork/tests/critical_world_journey.rs`:
   - First assert `app:pre-deploy` is `"up to date"` on the unchanged warm `up --json` (proving `M3.57` warm stamp reuse!) AND assert `post-deploy`'s output (or edit `Forkfile`'s `pre-deploy` message / invalidate stamp for a second `--json` check) proves live `"event": "log"` streaming for an executing hook!
3. Also audit `critical_fork_live.rs` and `critical_status_live.rs` (and run all 3 `slipfork` live suites on `165.232.86.174`!) so zero stale pre-`M3.57`/`M3.58` assertions remain in the tail of `make live-test`!

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --features slipfork/live -- -D warnings` exits `0`.
- `critical_world_journey`, `critical_fork_live`, and `critical_status_live` all pass `100%` on `165.232.86.174`.
