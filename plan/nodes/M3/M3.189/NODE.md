---
id: M3.189
title: "live-test purge aborts with BTRFS_SUBVOL_RDONLY subvolumes: CI exit 2 before suite 11"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Makefile, "scripts/**", TESTING_PLAN.md, tools/do_vm.py]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T23:40"
updated: "2026-11-26T02:26"
---

# Problem (gate-measured, PR-blocking)

`make live-test` on a cold Btrfs mount runs suites 1..10 and then **exits 2** at `purge_store()` (`Makefile:177-182`; same in `LIVE_RESET=1` at `:158-162`) right before `slipfork/critical_status_live`:

- `rm -rf "$store"` floods ~344k `Read-only file system` lines;
- `btrfs subvolume delete -R "$store"` fails because `.slipfork-store` is a plain dir whose child subvolumes (`snapshots/*`, `images/oci/*`, `templates/postgres-18`) still carry `BTRFS_SUBVOL_RDONLY` (`ro=true`).

All 16 suites pass individually on cold mounts (16/16, 53/53 tests), so this is purely the purge path — and it deterministically reds `.github/workflows/ci.yml:127`.

# Fix

- Enumerate every subvolume under `$store` deepest-first (`btrfs subvolume list -o` or `find "$store" -depth -inum 256`), clear `btrfs property set -ts <subvol> ro false`, `btrfs subvolume delete` each, then `rm -rf` the remainder — in both `purge_store()` and `LIVE_RESET=1`.
- No error flood: capture/quiet btrfs output; on partial failure print one line + `next:` instead of 344k lines.
- Order `LIVE_COLD_SUITES` first in `LIVE_SUITES` so CI does not re-pull the cold base image twice.
- Keep `guard.py`/`tools` green; `docs/` untouched.

# Acceptance

- On a cold VM mount: a **single** `make live-test` invocation exits 0 with 16/16 and no read-only flood; `LIVE_RESET=1 make live-test` also exits 0 and leaves the store absent.
- Simulated warm store still refuses with the one-line remedy (M3.185 behaviour intact).
- `make check`; Triad-light (`review-tests`); commit `[M3.189] ...`, done, no push.
