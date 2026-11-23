---
id: M3.52
title: "Flush Postgres shared_buffers via <5ms direct-TCP wire CHECKPOINT before fork syncfs & align specs/03 CDN prose"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork/**"
  - "specs/03-under-the-hood.md"
  - ARCHITECTURE.md
  - .opencode/LESSONS.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T18:27"
updated: "2026-11-22T19:13"
---

# Goal

Fix the `H-012` live defect (`critical_fork_live` seeing 3 rows instead of 4 in the fork when a parent `INSERT` was committed `< 200ms` before `slipfork fork`) in `< 5ms` without `crun exec` overhead, and align `specs/03-under-the-hood.md` + `ARCHITECTURE.md` with `specs/01-forkfile.md` §4 (`cdn <id>` aliases `<bucket>.<world>.localhost` locally).

# Exact Root Cause & Fix

1. **Why `syncfs` alone missed a freshly committed `INSERT` (`3` vs `4` rows):**
   - With `synchronous_commit=off` and `wal_writer_delay=200ms`, a freshly committed PostgreSQL transaction sits in PostgreSQL's **user-space `wal_buffers` / `shared_buffers`** before `postgres` even calls `write(2)` to kernel page cache — so `syncfs(2)` alone cannot flush bytes that haven't entered the kernel yet!
   - Earlier `crun exec psql -c CHECKPOINT` was slow (`130ms`) ONLY because it spawned `crun exec` + namespaces + `psql`.
   - **Fast `< 5ms` fix:** Before `syncfs` in `fork`, for each live `postgres` resource in the parent world, dial `127.0.0.1:<pg_port>` directly via `Request::Dial { port }` over `<pods_dir>/control.sock` (`SCM_RIGHTS`, `< 0.5ms`, zero subprocess!) and speak minimal PostgreSQL v3 wire protocol over that `TcpStream`/`UnixStream`:
     - Send `StartupMessage` (`version = 196608`, `"user\0postgres\0database\0postgres\0\0"`, or password/cleartext/MD5/SCRAM if configured — check `PostgresLauncher` auth config: `trust` on `127.0.0.1`!), wait for `ReadyForQuery` (`'Z'`),
     - Send Simple Query `'Q'` (`"CHECKPOINT;\0"`), wait for `'C'` + `'Z'` (`ReadyForQuery`), and close `Terminate` (`'X'`).
     - Because `postgres` runs with `fsync=off`, `CHECKPOINT;` writes dirty `shared_buffers` + `wal_buffers` into kernel page cache via `write(2)` in **`~1–3ms`** with zero disk `fdatasync`, and then `syncfs` commits them into the Btrfs tree!
2. **Align `specs/03-under-the-hood.md` & `ARCHITECTURE.md`:**
   - Remove the stale mention of separate `static-cdn.<world>.localhost` virtual-host dispatch in `specs/03-under-the-hood.md` / `ARCHITECTURE.md` so all specs match `specs/01-forkfile.md` §4 (`s3 <bucket>` always serves `<bucket>.<world>.localhost`, and `cdn <id>` aliases `<bucket>.<world>.localhost` locally).
3. **Prove Live on `165.232.86.174` & Update `.deb`:**
   - Run **all 7 live suites** (`critical_fork_live` full journey included!) on `165.232.86.174`, rebuild `make packages`, and install the updated `.deb` on `165.232.86.174` leaving `demo`, `feat-auth`, and `youpi` clean!

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` exit `0`.
- Full `critical_fork_live` (including immediate parent `INSERT` -> `slipfork fork` -> 4 rows in fork + `< 1.0s` lab fork budget) passes `100%` on `165.232.86.174`, and the updated `.deb` is installed on `165.232.86.174`.
