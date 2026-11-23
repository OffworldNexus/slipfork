# Log M3.28.1

## 2026-11-17T16:32 [planner]

created (task) DX of record for the 0.1.0 UX pass: bare status, restart, ps, open, fork naming, and the :80 ingress

## 2026-11-17T16:34 [lead]

status ready -> active

## 2026-11-17T16:34 [lead]

attempt 1/3 started

## 2026-11-17T16:34 [lead]

base 1d4c08e7ee38e5df2b631daf9c62bd345b854a52

## 2026-11-17T17:53 [lead]

status active -> review

## 2026-11-17T17:56 [lead]

round 2 (quad gate): consolidated architect fix pass — one command table + one --json schema home (dx normative, target-dx links), one wire model (IngressInfo/WorldSummary.parent/WorldProcessReport/WorldServiceSummary with port+services+ServiceState, no daemon-wide lineage), WorldOrigin collapsed to a struct, UserIdentity::invoking_account, Layout::for_account, UnitInstaller narrow verbs, single BrowserOpener in crates/slipfork/src/browser.rs, open --remote uses ingress.port with constant 8080, flat AppArmor alternation, validate_fork_name passes real domains, ADR-0011 tense, one boot pipeline for up/restart, dag types not drawn in the 0.1.0 CRD

## 2026-11-17T18:22 [lead]

round-2 confirmation: PASS (acceptance green, 10/10 blocker checks closed). Logged MINORs: ARCHITECTURE.md:560 stale WorldOrigin::Local comment -> fixed in a follow-up; ADR-0005 present-tense share claims -> folded into M3.28.15 (path added).

## 2026-11-17T18:30 [lead]

status review -> done

## 2026-11-17T18:30 [lead]

pre-commit gates green: plan.py validate, guard.py, critical_guard.py (no protected test touched), make check rc=0 (incl. docs-check + check_docs_honesty)
