# Log M3.28.16

## 2026-11-17T17:41 [planner]

created (task) Guard rail: production composition roots may not wire stub adapters (tools/check_stub_adapters.py)

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T03:11 [lead]

status ready -> active

## 2026-11-18T03:11 [lead]

attempt 1/3 started

## 2026-11-18T03:11 [lead]

base 4a1428f1752420e57399c20f61db0dcec3d08ec3

## 2026-11-18T03:16 [lead]

status active -> ready

## 2026-11-18T03:16 [lead]

attempt 1: tool+tests+LESSONS+Makefile wiring delivered and verified (8/9 pytest; real-repo case correctly RED). Blocked by SEQUENCING, not by a missing input: it is wired into 'make check' and reports the vapour that M3.28.15 exists to delete (UnavailableTunnelRegistrar in daemon.rs:34/778). I activated it before .15 — my error. Parked; .15 runs next, then .16 re-runs and turns green with no code change.

## 2026-11-18T03:48 [lead]

status ready -> done

## 2026-11-18T03:48 [lead]

un-parked and re-armed after M3.28.15: tool exit 0 (91 production files, 6 composition roots), pytest 9 passed incl. test_real_repository_is_clean, stub-check back in make check. Its own make check run was green through rustdoc/doc-tests and then hit M3.28.10's mid-edit compile error in main.rs (ForkOptions.fork), not this node; re-verified at the epic gate.
