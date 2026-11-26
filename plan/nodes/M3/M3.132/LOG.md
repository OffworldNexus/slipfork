# Log M3.132

## 2026-11-24T22:02 [conductor]

created (task) Fork/drop state UX defects: per-frame counters in drop, ambiguous nested fork names, stale and half-created fork entries

## 2026-11-25T00:05 [conductor]

Superseded by later work: drop's single plan total landed in M3.133 (7ba3512); duplicate-fork refusal and the .forks-in-checkout snapshot leak landed in M3.129/M3.120 (0e03d00, a419321); nested-name ambiguity and half-created ghosts are removed by construction in the ForkLedger epic M3.134 (flat <root>/.forks/<name>, ledger-owned rows, plan_converge). Nothing unique remains.

## 2026-11-25T00:05 [conductor]

status ready -> superseded
