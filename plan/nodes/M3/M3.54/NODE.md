---
id: M3.54
title: "Fix slipfork ps when root world is down: include running child .forks/* worlds & stopped root/forks in project scope"
kind: task
status: active
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/**", "specs/02-cli-ux.md", docs/cli.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T20:59"
updated: "2026-11-22T21:00"
---

# Goal

Fix `slipfork ps` (and bare `slipfork`) when the root world (`~/demo`) is `down` while one or more `.forks/*` (`~/demo/.forks/feat-auth`) are still running (or stopped on disk):

# Exact Root Cause (`crates/slipfork/src/status/tree.rs:55-85` & `crates/slipfork/src/daemon/lifecycle/ps.rs:70-88`)

1. When `demo` (`~/demo`) is stopped via `slipfork down` while `feat-auth` (`~/demo/.forks/feat-auth`) stays running:
   - `demo` leaves `self.worlds`, so `ps.rs:81` places `feat-auth` (`world.path = "/home/slipfork/demo/.forks/feat-auth"`) at the top level of `reply.worlds` (and `attach_stopped` never scans `/home/slipfork/demo/.forks`, losing `youpi` too!).
   - Then `status::scope(&reply, cwd)` (`tree.rs:76`) only checked `if cwd.starts_with(&world.path)` — and `/home/slipfork/demo` does **NOT** start with `/home/slipfork/demo/.forks/feat-auth`! So `scope` returned **empty** (`no world running in ~/demo`) while `feat-auth` was still running right inside `~/demo/.forks/feat-auth`!

# Exact Fix

1. **Parent-project vs inside-fork scoping (`status::scope` & `commands::mod::render_status`):**
   - Resolve the canonical project root containing `cwd` (walking up to the nearest directory with `Forkfile` or `.slipfork` / Btrfs subvolume).
   - **If `cwd` is inside a specific fork (`~/demo/.forks/feat-auth[/...]`):** scope strictly to that fork (`feat-auth`, running or `○ feat-auth ... stopped` if that fork is stopped on disk!).
   - **If `cwd` is in the root project (`~/demo[/app]` outside `.forks/`):**
     - Include the root world if running (`● demo ~/demo`) — OR if the root world is stopped on disk (has `Forkfile`), show a dim stopped row `○ demo  ~/demo  stopped`!
     - Include **every running fork** whose `world.path` is under `<project_root>/.forks/` (`world.path.starts_with(&project_root.join(".forks"))`)!
     - Discover and include **every stopped fork** in `<project_root>/.forks/*` (even when the root world itself is `down` or when the daemon isn't even running yet!).
2. **Bare `slipfork` (`status`, unscoped) when root world is `down` but a fork is running:**
   - For any running fork `feat-auth--demo` whose parent `<parent_project>` is `down`, also discover sibling stopped forks in `<parent_project>/.forks/*` (and show `○ demo ~/demo stopped` or sibling stopped forks cleanly without duplicating rows!).
3. **E2E Test + Live `165.232.86.174` Update:**
   - Add a real E2E test: `up` root + `fork feat-auth` + `fork youpi` + `down youpi` + **`down` root (`demo`)** -> run `slipfork ps` in `~/demo` -> assert `○ demo ... stopped`, `○ youpi ... stopped`, AND `● feat-auth ... running` (with all its domains & services) are ALL shown! Also assert `cd .forks/youpi && slipfork ps` shows `○ youpi ... stopped` and `cd .forks/feat-auth && slipfork ps` shows only `● feat-auth`!
   - Rebuild `.deb` (`make packages`), install on `165.232.86.174`, and verify `slipfork down` -> `slipfork ps` -> `slipfork up` live in `/home/slipfork/demo`!

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` all exit `0`.
- Live verification on `165.232.86.174`: running `slipfork down` in `~/demo` followed by `slipfork ps` shows `○ demo ~/demo stopped`, `○ youpi ~/demo/.forks/youpi stopped`, and `● feat-auth ~/demo/.forks/feat-auth` running (then `slipfork up` brings `demo` right back up).
