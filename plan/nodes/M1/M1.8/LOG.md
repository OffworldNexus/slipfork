# Log M1.8

## 2026-11-15T10:38 [conductor]

created (task) Fold owner review-gate corrections: single-origin invariant, support tiers, typed coverage (no shell recipes)

## 2026-11-15T10:39 [lead]

status ready -> active

## 2026-11-15T10:39 [lead]

attempt 1/3 started

## 2026-11-15T10:39 [lead]

base 05c85939f7514d9286691b6c8e01e10055bdba64

## 2026-11-15T10:39 [lead]

owner clarification (addendum): world origin is NOT an OSPP value; Slipfork-assigned origin wins over provider-supplied domains:: for local worlds; OSPP is NOT a prerequisite for M2 (FileVaultProvider + derived .localhost first); modes.local must carry no hardcoded BASE_URL literal. Fold into ADR-0005/ADR-0006 + M2 roadmap row.

## 2026-11-15T10:47 [architect]

edited fields

## 2026-11-15T10:58 [lead]

status active -> review

## 2026-11-15T11:00 [lead]

GATE(1) partial: legality=PASS(0 BLOCKER, 4 MAJOR, 4 MINOR); cruft=FAIL(BLOCKER: crates/slipfork-dag TestSpec.after_suite dead field still parsed + docs call it 'unreferenced placeholder'; +3 MAJOR run-dir/origin-constructor/claim, 4 MINOR). BLOCKER is outside M1.8 path lock (docs-only node) -> remediation round: in-scope doc accuracy fixes, code sweep deferred to follow-on node.

## 2026-11-15T11:00 [lead]

attempt 2/3 started

## 2026-11-15T11:00 [lead]

status review -> active

## 2026-11-15T11:00 [lead]

GATE(1) complete: reviewer=PASS, legality=PASS, ioc=FAIL SPEC_BLIND_SPOT (shared ingress/weaved target/308 loop -> H-008 filed, Option A recommended), cruft=FAIL (dead [test] field still parsed in crates + false 'unreferenced' claim). Remediation attempt 2/3 -> architect.

## 2026-11-15T11:09 [lead]

status active -> review

## 2026-11-15T11:11 [lead]

GATE(2) FINAL: reviewer=PASS(0 BLOCKER), legality=PASS(0), ioc=PASS(0, round-1 SPEC_BLIND_SPOT dissolved by H-008), cruft=PASS(0, residual crate residue tracked). Merged MAJOR/MINOR -> FOLLOWUPS.md. Created M1.9 (required pre-0.1.0 code sweep).

## 2026-11-15T11:11 [lead]

status review -> done
