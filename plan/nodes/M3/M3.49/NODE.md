---
id: M3.49
title: "Update specs/ & ARCHITECTURE.md: unified PTY slipfork shell (drop db/redis), DECSTBM fixed-bottom-bar 1-line-per-task TUI & <1s 8-component fork"
kind: task
status: done
owner: architect
priority: 3
depends_on: []
paths: ["specs/**", TESTING_PLAN.md, ARCHITECTURE.md, .opencode/LESSONS.md]
attempts: 2
max_attempts: 3
escalation: architect
created: "2026-11-22T12:10"
updated: "2026-11-22T12:39"
---

# Goal

Update `specs/02-cli-ux.md`, `specs/03-under-the-hood.md`, `specs/04-testing-plan.md`, `TESTING_PLAN.md`, `ARCHITECTURE.md`, and `.opencode/LESSONS.md` to lock in the owner's three UX & performance requirements cleanly:

1. **Unified `slipfork shell <target>` Over a Real Kernel PTY — Delete `slipfork db` & `slipfork redis` Completely:**
   - Remove `slipfork db` and `slipfork redis` completely (pre-`1.0` scorched-earth deletion — zero aliases).
   - `slipfork shell <target>` requires `<target>` and dispatches by component kind:
     - `service` or `image` (`slipfork shell api`, `slipfork shell backend`) -> interactive `/bin/bash` if present in the container rootfs (clean `(<world>:<target>) \w$ ` prompt), falling back to `/bin/sh`.
     - `postgres` cluster or declared `database` (`slipfork shell pg`, `slipfork shell app_db`) -> `psql` connected directly to that database inside the `postgres` container (interactive PTY on TTY; `psql -X -q -v ON_ERROR_STOP=1` when `stdin` is piped).
     - `redis` resource (`slipfork shell cache`) -> `redis-cli` against that Redis instance (interactive PTY on TTY; piped stdio when non-TTY).
     - `s3` bucket (`slipfork shell uploads`) -> clear 1-line refusal + `next:` listing shellable targets.
   - **Real PTY (`crun exec --tty --console-socket` + `SCM_RIGHTS` + host raw mode + `SIGWINCH`):**
     - When CLI `stdin` + `stdout` are TTYs, `crun exec --tty --console-socket` creates a real `/dev/pts/N` controlling terminal inside the container (`setsid` + `TIOCSCTTY` — zero `sh: 0: can't access tty; job control turned off`!), passes the PTY master fd via `SCM_RIGHTS`, puts the host terminal in raw mode (`RawModeGuard` restoring cooked mode on `Drop`), and forwards initial + `SIGWINCH` `(cols, rows)` via `ioctl(TIOCSWINSZ)`.

2. **`apt`-Style Fixed Bottom Progress Panel (`DECSTBM` Scroll Region) + 1-Line-Per-Task Flow + Crisp Failure Remedy (`specs/02-cli-ux.md`):**
   - Replace `indicatif::MultiProgress` dynamic spinner insertion/removal (which slid down the screen and bounced height) with a true **VT100 `DECSTBM` scroll region (`\x1b[1;{H-1}r`)**:
     - **Row `H` (very bottom line of the terminal window, fixed 1-line height, never moves vertically):**
       `⠋ [████████████░░░░░░░░░░░░]  8/13  62% · Starting api, worker… · 0.5s`
       (Handles `SIGWINCH` resize and resets `\x1b[r` + clears row `H` cleanly via RAII `Drop` / `SIGINT` guard before returning to the shell prompt.)
     - **Regular terminal flow (`rows 1..H-1`) — strictly ONE line per task:**
       - Zero `  │ [3/11] sealed sha256:...` or build/hook log lines in the terminal! All build/hook/service output goes strictly to per-component log files (`slipfork logs <image|service|resource>`).
       - Truthful status words: cached OCI images & cached `Dockerfile` images print dim `• image <name> cached <ms>` (NEVER `pulled` or `built` on cache hit!).
     - **On error:** Print the failed task row (`✗ image backend failed 84ms`) followed by ONE crisp remedy line:
       `✗ Failed to build image backend — check logs with: slipfork logs backend` (and ensure `slipfork logs <image>` reads image-build logs too!).

3. **True `< 1.0s` End-to-End `slipfork fork` on Full 8-Component Stacks (`specs/03-under-the-hood.md`):**
   - Delete pre-fork `crun exec ... CHECKPOINT` (`~20ms` crash-consistent Btrfs snapshot with `fsync=off`).
   - Bypass `docker-entrypoint.sh` bash wrappers on already-initialized `PGDATA` / Redis dirs — exec `postgres` and `redis-server` binaries directly (`~50ms` PG, `~15ms` Redis).
   - On `fork`, reuse parent world's resolved `image` subvolumes directly (`cached`, `~1ms`), and when a service's `before-start` / `pre-deploy` hooks are `up to date`, spawn `service` containers **in parallel** with `pg`/`redis` instead of waiting serially behind `pg` readiness!

# Acceptance

- `specs/02-cli-ux.md`, `specs/03-under-the-hood.md`, `specs/04-testing-plan.md`, `TESTING_PLAN.md`, and `ARCHITECTURE.md` have zero `slipfork db` / `slipfork redis` references, specify the `DECSTBM` fixed bottom panel + 1-line-per-task flow + real PTY `shell`, and pass `uv run --no-project tools/guard.py`.
