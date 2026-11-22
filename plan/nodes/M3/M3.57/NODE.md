---
id: M3.57
title: "True & visible parallelism: warm-up stamp reuse (not is_fork-only), parallel pod stop on down & live in-place spinner rows above DECSTBM bar"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.55]
paths: ["crates/slipfork-world/**", "crates/slipfork/**", "specs/02-cli-ux.md", "docs/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T21:44"
updated: "2026-11-22T21:44"
---

# Goal

Fix all three places where container startup/shutdown is still sequential (or hides concurrent work behind sequential-looking completions):

1. **Real Parallelism on Warm `slipfork up` (`crates/slipfork/src/world/supervise.rs:82-87` & `runtime.rs:86`):**
   - Currently `supervise.rs:83` (`if is_fork { StampStore::for_fork(...) } else { StampStore::gated(...) }`) and `runtime.rs:86` (`if is_fork { ... adopted ... }`) ONLY reuse `WorldStampStore` (`pre-deploy` stamp + sealed image subvolumes) when `is_fork == true`!
   - Consequently, running `slipfork down` -> `slipfork up` on a root world whose `Forkfile`, lockfiles, and `PGDATA` haven't changed STILL re-runs `pre-deploy` unconditionally, forcing `pg` -> `api:pre-deploy` -> `service api` into a **3.3s serial chain**!
   - **Fix:** Reuse trusted `WorldStampStore` (`world_epoch` + `pg_clones` checkpoint stamp match) on **both** `fork` AND warm `up` (`!is_fork` when `<store>/worlds/<world>` already has a matching `WorldStamp`)! Then warm `slipfork up` skips unchanged `pre-deploy` (`• up to date <1ms`) and launches `pg`, `cache`, `api`, `worker`, `front`, `mailpit` **all simultaneously at `t = 0`**!

2. **Real Parallel Pod Teardown on `slipfork down` (`crates/slipfork-world/src/bin/world_init/pod/stop.rs` & `daemon/lifecycle/down.rs`):**
   - Stop pods **concurrently** (`thread::scope` / parallel `crun kill` + `wait` + `crun delete` across all running pods) instead of iterating pods one-by-one in a serial loop!

3. **Visually Obvious Parallelism in `BootTui` (`crates/slipfork/src/boot/`):**
   - Currently `Pane::start` (`pane.rs:137-140`) prints NOTHING when 6 nodes start simultaneously and overwrites `self.summary = format!("Starting {name}…")` with only the single most recent node — so tasks appear one-by-one only as they finish, making parallel execution look 100% sequential!
   - **Fix:**
     - On `Started`, immediately allocate that task's **own single row** in the visible task board above the `DECSTBM` bottom bar showing a live cyan Braille spinner (`⠋ service   front          starting…`, `⠋ service   api            starting…`, `⠋ resource  pg             starting…` — all concurrently active tasks spinning at the same time, updating elapsed `0.3s` on tick!), and when that task finishes (`Finished` / `UpToDate` / `Failed`), update **that task's own row in place** to `✓ service   front          ready      420ms` (still strictly 1 line per task, fixed row slot per task, preserving the 2D VT100 prompt-safety invariant!).
     - On the fixed bottom `DECSTBM` bar (row `rows`), show all active tasks concisely (e.g. `⠋ [████████░░░░░░░░░░░░]  6/12  50% · 4 running (pg, front, worker, api) · 0.4s`) instead of overwriting with just `"Starting api…"`!

# Acceptance

- `make check` (including the 2D VT100 bottom-row & mid-screen prompt preservation tests!), `make docs-check`, `make packages`, and `make packages-test` exit `0`.
- Live verification on `165.232.86.174` (`~/demo`):
  1. Warm `slipfork down && slipfork up` marks `api:pre-deploy` `up to date` and boots `pg`, `cache`, `api`, `worker`, `front`, `mailpit` **concurrently** (verified by overlapping start timestamps & concurrent spinner rows on PTY!).
  2. `slipfork down` stops all pods in parallel.
