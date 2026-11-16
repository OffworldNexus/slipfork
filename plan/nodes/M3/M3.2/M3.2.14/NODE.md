---
id: M3.2.14
title: "Makefile: run pytest-slipfork tests in py-test and serialize new world-booting suites"
kind: task
status: ready
owner: implementer
priority: 4
depends_on: [M3.2.12, M3.2.11]
paths: [Makefile]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Extend the Makefile gate for `0.3.0`: run the `pytest-slipfork` plugin tests inside `py-test`, keep the new world-booting `slipfork-test` suites serialized, and keep `make check` deterministic with zero environment overrides.

# Context

- `M3.1` set `TMPDIR`/`CARGO_BUILD_JOBS` and serialized the existing world-booting suites. `M3.2.x` adds a Python package (`packages/pytest-slipfork`) and Rust suites that lease Btrfs worlds, neither of which the gate runs yet.

# Design / Invariants

1. `py-test` runs both `tools/tests` and `packages/pytest-slipfork/tests` (same `uv run --no-project --with pytest` invocation shape, no new tooling).
2. `rs-test` serializes every binary that boots worlds (engine live suites, `slipfork-test` integration suites, `slipfork` live suites) with `--test-threads=1`, one binary at a time; pure suites stay parallel.
3. Keep `TMPDIR`/`CARGO_BUILD_JOBS` invariants from `M3.1` untouched; no `/tmp` reintroduction; no `sleep` padding.
4. `make check` remains the single documented gate; `bench` stays opt-in and unchanged.

# Acceptance

```bash
grep -n 'packages/pytest-slipfork/tests' Makefile
grep -n 'test-threads=1' Makefile
grep -n 'CARGO_BUILD_JOBS ?= 4' Makefile
! grep -nE 'TMPDIR[^\n]*/tmp(/|$)' Makefile
make py-test
make rs-test
make check
```

Expected: the three `grep`s find the plugin tests, the serialized thread flag, and the job cap; the inverted `grep` prints nothing; `make py-test`, `make rs-test`, and `make check` exit 0 with zero environment overrides.
