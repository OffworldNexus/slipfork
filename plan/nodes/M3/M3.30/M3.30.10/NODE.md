---
id: M3.30.10
title: Keep the do_vm harness unit test aligned with the consolidated SUITES list
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.30.4]
paths: [tools/tests/test_do_vm.py]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T06:25"
updated: "2026-11-19T06:37"
---

# Goal
`make check` must pass: `tools/tests/test_do_vm.py::test_test_streams_every_suite_serialized_and_stops_at_first_failure` hardcodes the pre-consolidation 5-suite list.

# Evidence
`make check` -> py-test fails: expected [btrfs_live, btrfs_live, cli, slug, ospp_daemon] but `do_vm_mod.SUITES` now carries 11 entries (engine btrfs_live, engine, oci, edge_live, slipfork btrfs_live, cli, l0, ospp, slug, world, vcs).

# Fix
Make the assertion derive its expectation from `do_vm_mod.SUITES` (package/test pairs, order preserved) instead of a hardcoded literal, so the next registered suite cannot silently break the harness test. Keep the serialized `--test-threads=1` and stop-at-first-failure assertions.

# Acceptance
`uv run --no-project --with pytest pytest -q tools/tests` exits 0; `make check` exits 0.
