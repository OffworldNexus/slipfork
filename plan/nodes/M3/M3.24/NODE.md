---
id: M3.24
title: "Finish bounded-wait consolidation, reap orphaned world-init processes, and make rs-test-list comments match what the guard enforces"
kind: task
status: done
owner: implementer
priority: 4
depends_on: [M3.1]
paths:
  - Makefile
  - poc/run_benchmark.py
  - crates/slipfork/tests/critical_l0_wiring.rs
  - crates/slipfork/tests/critical_slug.rs
  - crates/slipfork/tests/critical_cli.rs
  - "crates/slipfork-engine/tests/critical_engine.rs"
  - crates/slipfork/tests/support/mod.rs
  - "crates/slipfork-engine/tests/support/mod.rs"
  - "crates/slipfork-testkit/src/lib.rs"
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/main.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T20:44"
updated: "2026-11-17T13:42"
---

# Goal

Finish the bounded-wait consolidation started in `M3.1`: every world-booting suite harness in `crates/slipfork` and `crates/slipfork-engine` uses one shared `poll_until` primitive with identical signature and named deadlines, and the `Makefile` `rs-test-list` comments state exactly what the guard enforces (and nothing more).

# Context

- `M3.1` introduced `poll_until` per crate and routed the daemon waits and the `critical_oci_runtime` readiness probes through it, but two hand-rolled `Instant`/`thread::sleep` loops with unnamed deadlines remain outside its `paths`: `crates/slipfork/tests/critical_l0_wiring.rs:172` and `crates/slipfork-engine/tests/critical_engine.rs:98`.
- The two `poll_until` copies already diverge in parameter naming (`bound` vs `deadline`), so a later shared test-support seam would be a rewrite instead of a move.
- `Makefile`'s guard comment claims more than it enforces: `WORLD_BOOTING_TESTS` is an explicit but human-maintained classification, and `TMPDIR :=` intentionally overrides a caller's value (acceptance-mandated literal).

# Design / Invariants

1. Give both crate harnesses an identical `poll_until(bound: Duration, interval: Duration, probe: impl FnMut() -> Result<(), String>) -> Result<(), String>` signature (one parameter name), with one documented poll interval per crate.
2. Convert the two remaining hand-rolled loops to it, preserving each existing deadline value and every assertion; no new `sleep()` padding, no weakened timeouts.
3. Correct the `Makefile` comments to match reality: the guard proves list↔filesystem parity, uniqueness, and `--test-threads=1` on every classified world-booting binary; the classification itself is explicit and human-maintained. Correct the `TMPDIR :=` comment to say it deliberately fixes the harness scratch root (callers can still pass `TMPDIR=` on the command line).
4. Touch only the four harness files plus `Makefile`; production code is untouched.

# Acceptance

```bash
! grep -rn 'thread::sleep' crates/slipfork/tests/critical_l0_wiring.rs crates/slipfork-engine/tests/critical_engine.rs
grep -n 'pub fn poll_until' crates/slipfork/tests/support/mod.rs crates/slipfork-engine/tests/support/mod.rs
make rs-test-list
make check
```

Expected: no hand-rolled sleep loop survives in the two converted suites; both crates expose `pub fn poll_until` with identical parameter names; `make rs-test-list` and `make check` exit 0.
