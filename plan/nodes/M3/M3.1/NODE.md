---
id: M3.1
title: "Serialize world-booting Rust critical suites in Makefile rs-test"
kind: task
status: ready
owner: implementer
priority: 1
depends_on: []
paths: [Makefile]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T06:34"
---

# Goal

Make `make check` (`rs-test`) deterministic under shared-host CPU/IO load without requiring `RUST_TEST_THREADS=1` in the caller's environment.

# Context

- At the `0.2.0` (`M2`) milestone gate, `RUST_TEST_THREADS=1 make check` passed 100%, while default-thread `cargo test --workspace --all-targets` intermittently red-lined world-boot teardown in `critical_slug` / `critical_oci_runtime` when multiple test cases concurrently booted real `postgres:18` `crun` containers on a loaded shared host (both pass reliably in isolation).
- Keep fast pure/unit suites running in parallel while passing `-- --test-threads=1` (or equivalent serialized cargo invocation in `Makefile`'s `rs-test` target) for the world-booting integration suites (`slipfork` and `slipfork-engine` integration binaries, or `cargo test --workspace --all-targets -- --test-threads=1` if wall time stays bounded).

# Design / Invariants

1. Touch **only** `Makefile` (`rs-test`). Never weaken timeouts, delete assertions, or add `sleep()` padding to `critical_*.rs`.
2. Plain `make check` (with zero environment overrides) must run every unit, critical, doc, TS, Python, support, plan, guard, and Zensical check and exit `0` deterministically across back-to-back runs.

# Acceptance

```bash
make rs-test
make check
```
