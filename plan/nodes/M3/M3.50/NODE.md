---
id: M3.50
title: "Implement unified PTY slipfork shell (drop db/redis), DECSTBM fixed-bottom-bar TUI & <1.0s 8-component ~/demo fork on 165.232.86.174"
kind: epic
status: active
priority: 3
depends_on: [M3.49]
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
created: "2026-11-22T12:10"
updated: "2026-11-22T12:40"
---

# Goal

Implement every item in `M3.49` cleanly across code, docs, manpage/completions, tests, and live `165.232.86.174` `/home/slipfork/demo`:

1. **Unified Real-PTY `slipfork shell <target>` & Scorched-Earth Deletion of `db` / `redis`:**
   - Delete `slipfork db` and `slipfork redis` from `clap` CLI, commands, docs (`docs/**`, `README.md`, `support/demo/README.md`), completions, and `packaging/man/slipfork.1`.
   - Implement real kernel PTY execution in `slipfork-world` (`crun exec --tty --console-socket` + `SCM_RIGHTS` PTY master fd handoff + `TIOCSWINSZ` resize control frame) and raw-mode TTY bridging in `crates/slipfork` (`crossterm` `RawModeGuard` restoring cooked mode on `Drop`, `SIGWINCH` forwarding).
   - Route `slipfork shell <target>` across `service` / `image` (`/bin/bash` preferred with `(<world>:<target>)` `PS1`, fallback `/bin/sh`), `postgres` cluster or `database` (`psql` interactive PTY or piped `-X -q -v ON_ERROR_STOP=1`), and `redis` (`redis-cli` interactive PTY or piped).
   - Also support `slipfork logs <image>` so `check logs with: slipfork logs <image>` works whenever an image build fails!

2. **True `apt`-Style `DECSTBM` Fixed Bottom Panel + Strictly 1-Line-Per-Task Flow:**
   - Replace `indicatif::MultiProgress` in `crates/slipfork/src/boot/` with a clean `DECSTBM` (`\x1b[1;{rows-1}r`) bottom-row progress bar renderer (`crossterm` terminal size + `SIGWINCH` + RAII `ScrollRegionGuard` clearing row `rows` and resetting `\x1b[r` on `Drop` / `SIGINT`).
   - Suppress all `StepLog` (`[3/11] sealed sha256:...` / `dockerfile ...`) lines from terminal output — write build/hook logs strictly to per-component log files and print **strictly 1 aligned line per task** in the regular scroll region above row `rows`, with truthful status words (`cached`, never `pulled` or `built` on cache hits!).
   - Format build/hook/service failures cleanly as:
     `✗ Failed to build image <name> — check logs with: slipfork logs <name>` (or `Failed to run hook ...` / `Failed to start service ...`).

3. **Real `< 1.0s` End-to-End `slipfork fork` on the Full 8-Component `~/demo` (`165.232.86.174`):**
   - Delete `crates/slipfork-world/src/resources/pg/checkpoint.rs` pre-fork `CHECKPOINT` (`~20ms` Btrfs snapshot).
   - Exec `postgres` (`/usr/lib/postgresql/18/bin/postgres` / image `postgres`) and `redis-server` directly on already-initialized `PGDATA`/Redis directories, skipping `docker-entrypoint.sh` bash overhead.
   - Reuse parent world's resolved `image` subvolumes directly on `fork` (`cached` in `~1ms`), and in the Boot DAG start `service` containers **in parallel** with `pg`/`redis` whenever `before-start` and `pre-deploy` hooks are `up to date` (also use `--no-sync` after `@uv` setup or `.venv/bin/...` in `/home/slipfork/demo/Forkfile` so runtime `uvicorn`/`worker` don't re-run `uv` lock checks!).
   - Rebuild & install the fresh `.deb` on `165.232.86.174`, run all 7 live Btrfs critical suites (including real PTY `slipfork shell` tests verifying `isatty` + zero `can't access tty` across `api`, `app_db`, and `cache`), refresh `/home/slipfork/demo` + `feat-auth` (cleaning up any leftover `tralala` fork!), and measure **actual `time slipfork fork probe-speed` inside `/home/slipfork/demo` (all 8 components)** proving **total CLI wall-clock `< 1.0s`** (then `slipfork drop probe-speed`)!

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` all exit `0`.
- Live `165.232.86.174` verification:
  1. `slipfork shell api`, `slipfork shell app_db`, and `slipfork shell cache` in a real PTY have real `/dev/pts/*` TTYs (`0` `can't access tty`), and `slipfork db` / `slipfork redis` do not exist.
  2. `slipfork fork` on the real 8-component `/home/slipfork/demo` completes in **`< 1.0s` total CLI wall-clock** with **1 line per task**, truthful `cached` labels, zero `sha256` debug lines, and the `DECSTBM` fixed bottom progress bar on row `H`.
