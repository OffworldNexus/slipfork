---
id: M3.51
title: "Fix DECSTBM prompt mangling (+ 2D VT100 grid test), atomic fork rollback, positional up/down/restart [name] & stopped .forks/* detection"
kind: epic
status: done
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - TESTING_PLAN.md
  - "specs/**"
  - "crates/**"
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - "packaging/**"
  - "scripts/**"
  - "support/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T17:21"
updated: "2026-11-22T18:25"
---

# Goal

Fix all four real-world CLI/TUI & lifecycle defects exposed on `165.232.86.174` (and lock Option A from `H-011` — keep `syncfs` with honest `~1.1s` 8-pod droplet median / `892ms` lab median):

1. **Fix `DECSTBM` Prompt Mangling (`crates/slipfork/src/boot/pane.rs`) + Clean `up`/`restart` Rows + Real 2D VT100 Screen-Grid Test:**
   - **Bug (`pane.rs:125`):** `write!(stdout, "\x1b[{};1H{text}\r\n", self.rows - 1)` hardcoded row `rows - 1` without `\x1b[2K` or cursor tracking, painting task lines directly over the user's `slipfork@...:~/demo$ slipfork up` prompt (`✓ image backend cached 2msipfork up`, `✓ service demo running 228msslipfork restart`) whenever the prompt started near the bottom of the terminal!
   - **Fix:**
     - Query cursor position (`crossterm::cursor::position()` / DSR `\x1b[6n`, with safe fallback): if cursor is on row `rows`, emit `\r\n` first so the prompt scrolls up and `rows - 1` is blank; install `\x1b[1;{rows-1}r` and place the flow cursor at `\x1b[{flow_row};1H`.
     - Print task rows at the natural flow cursor via `\r\x1b[2K{text}\r\n` (advancing `flow_row = (flow_row + 1).min(rows - 1)`), draw the bottom bar strictly inside `\x1b7\x1b[{rows};1H\x1b[2K{panel}\x1b8`, and on `restore()`/`Drop` clear row `rows`, reset `\x1b[r`, and place cursor at `\x1b[{flow_row};1H\x1b[2K` right below the last printed line.
     - Remove the nagging `world keeps running; use 'slipfork down' when finished` line after `✓ Ready -> ...`, and fix `slipfork restart` mislabeling the world name as `✓ service <world> running` (emit real restarted component rows!).
   - **Mandatory 2D VT100 Screen-Grid Test:** Implement a real 2D `(rows × cols)` VT100 terminal screen-buffer emulator in test support (handling `\r`, `\n`, `\x1b[r` `DECSTBM`, `\x1b[H`, `\x1b[2K`, `\x1b[K`, `\x1b7`/`\x1b8`). Pre-seed the bottom row (`row = rows`) AND a top row (`row = 3`) with `slipfork@slipfork-dev-vm:~/demo$ slipfork up\r\n`, feed real PTY output of `up`, `restart`, `down`, and `fork`, and assert the prompt line remains 100% intact with zero overwritten characters on any row!

2. **Atomic `fork` Rollback (`crates/slipfork/src/daemon/lifecycle/fork.rs:135-150` & `daemon/mod.rs:413-418`):**
   - **Bug:** When `session.run` failed mid-`fork`, `BootSession` left the supervisor + already-started sibling pods registered in `self.worlds` / `worlds.json`, and `discard_fork` unlinked `.forks/<name>` + `worlds/<fork>--<world>` WITHOUT calling `stop_world` or removing `fork_key` from `worlds.json`!
   - **Fix:** `discard_fork(&mut self, &prepared)` MUST call `self.stop_world(&fork_key, ...)` + `self.worlds.remove(&fork_key)` + `self.persist()` BEFORE `RuntimePods::purge` and `engine.drop_fork`. Prove via an E2E test where a fork's `pre-deploy` or `pg` fails after sibling services start -> zero surviving containers/supervisors, zero `worlds.json` entry, zero `.forks/<name>` remnant!

3. **Positional `[target]` on `up`, `down`, `restart` (Delete Ignored `--world`) + Stopped `.forks/*` Discovery & Missing/Invalid `Forkfile` Honesty in `ps`:**
   - Delete the broken `--world <NAME>` flag on `up`/`down`/`restart` (which `up_plan` ignored when `cwd` had a live world!).
   - Accept optional positional `[target]` on `slipfork up [world|fork]`, `slipfork down [world|fork]`, and `slipfork restart [target] [-s/--service <svc>]` (`target` resolves a fork in `<cwd>/.forks/<target>` or a known world/fork name; on `restart`, if `<target>` is a service in `cwd`'s world rather than a fork/world name, restart that service!).
   - In `ps` / `status` (`WorldView`):
     - Never `.ok().unwrap_or_default()` a missing checkout or broken `Forkfile` into fake `orphaned` services! If a registered world's `project` directory no longer exists on disk, reap its stale entry/supervisor automatically (or mark the world `checkout missing — run slipfork drop <name>`); if `Forkfile` fails `compile`, surface a 1-line `Forkfile invalid` error row while keeping live pods marked `running`.
     - Discover stopped forks in `<project>/.forks/*` (e.g. `feat-auth`, `youpi`) and display them cleanly as dim `○ <fork>  ~/demo/.forks/<fork>  stopped` so `slipfork up <fork>` or `slipfork drop <fork>` works directly from `~/demo`!
   - Sync `specs/02-cli-ux.md`, `docs/cli.md`, `packaging/man/slipfork.1`, completions, and `check_docs_honesty.py`.

4. **Rebuild `.deb` & Live `165.232.86.174` Cleanup + Verification:**
   - Reap stale `tralala` (`PID 1355243`) and delete leftover `pgfresh*` test subvolumes in `/home/slipfork/.slipfork-store/worlds` on `165.232.86.174`.
   - Install the freshly built `.deb` (`max-glibc <= 2.34`), verify `demo`, `feat-auth`, and `youpi` via `slipfork ps`, `slipfork up feat-auth`, `slipfork down youpi`, `slipfork restart`, and verify zero prompt mangling over a real SSH PTY at the bottom of the screen!

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` all exit `0`.
- 2D VT100 screen-grid test proves zero prompt overwrite across `up`, `down`, `restart`, and `fork` from both mid-screen and bottom-row prompts.
- Live `165.232.86.174` verification passes across all commands with clean `/home/slipfork/.slipfork-store/worlds` and `/home/slipfork/demo`.
