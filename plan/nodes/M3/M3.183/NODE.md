---
id: M3.183
title: "Sync specs/02-04, TESTING_PLAN.md & CHANGELOG.md [0.1.1] with append-only Report CLI, image-USER pods, ForkLedger & 16 live suites"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: [M3.181, M3.182]
paths:
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - "specs/04-testing-plan.md"
  - specs/README.md
  - "specs/06-distribution-and-docs.md"
  - TESTING_PLAN.md
  - CHANGELOG.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T19:22"
updated: "2026-11-25T21:53"
---

# Goal

Eliminate post-`8680e67` drift across `specs/02`–`04`, `TESTING_PLAN.md`, and `CHANGELOG.md` `[0.1.1]`.

> **Plan note on `docs/` (DO NOT edit `docs/` while owner edits are active):** Once the owner finishes editing `docs/`, sync `docs/` with `slipfork docker` (`login`/`logout`/`list`/`images`/`pull`/`build`), `up --no-cache`, `logs --last N` (`-f` implies 20), append-only boot output, image-`USER` runtime pods + `0022` umask, and flat `<root>/.forks/<name>` + parent-splice `drop` + `←` lineage `ps`, then run `make docs-check`.

# Drift List to Fix Here

1. **`specs/02-cli-ux.md:143,149-163,255,283,311,318,322-331`:** replace `DECSTBM` scroll-region (`\x1b[1;{rows-1}r`, row-`rows` panel, `\x1b[r` restore) prose & Mermaid with M3.152 append-only output (`\r\x1b[2K` single status line, zero scroll regions); align `WallZone::from_time_zone`/`named`/`fixed` (`:311`), `ConnectNotice::Ephemeral` (`:327`), and §9.5 post-Slice-4 completed state.
2. **`specs/03-under-the-hood.md:58,134,142,174,289-290,365-388`:** replace `:58,174` `$UID`/`$GID` runtime pod claim with M3.153 image `USER` (default `0:0` mapped 1:1 to host uid, Docker caps for root, `/home/slipfork` fallback for unprivileged); replace deleted `ScrollRegionGuard`/`AsciiLine`/`TableLayout` (`:134,142`) with `ReportRenderer`/`Pane`/`StreamRenderer`/`WallZone`; remove stale `"Until the daemon owns every pull (§6.7 slice 3)"` note (`:289-290`) and future-tense slice phrasing.
3. **`TESTING_PLAN.md:12,34-39,73` & `specs/04-testing-plan.md`:** make `TESTING_PLAN.md` and `specs/04-testing-plan.md` identical on the 16 `LIVE_SUITES` + 8 host critical suites, append-only PTY contract (`zero DECSTBM`), `Distribution v2` + `OSPP v1` loopback reference servers, `critical_registry_live`, `exec -i/--no-interactive`, `-F -dev` `boot_dag`, `SLIPFORK_WORLD_INIT` guard, and `pod_run_live` image-`USER` proof.
4. **`CHANGELOG.md:6-30`:** complete `[0.1.1]` with `BuildGraph` + `SnapshotStore` + `--no-cache`, `ForkLedger` (`state.db`, flat `.forks/<name>`, parent-splice `drop`, lineage `ps`), `slipfork docker`, image-`USER` pods + `0022` umask, `arg` `service.port`, append-only `Report` CLI, and `logs --last N`.

# Acceptance

- `diff -u TESTING_PLAN.md specs/04-testing-plan.md` exits `0`, `grep -rn "DECSTBM\|ScrollRegionGuard" specs/ TESTING_PLAN.md` returns 0 hits, and `tools/guard.py` + `tools/plan.py validate` exit `0`.
