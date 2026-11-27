---
id: M3.187
title: tools/tests guard test still expects the boot.rs raw print that M3.182.4 removed
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["tools/tests/**", tools/guard.py]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T22:35"
updated: "2026-11-25T22:43"
---

# Problem

`tools/tests/test_guard_rejects_raw_print_outside_the_renderer_allowlist` fails at base `cfb73c0`: `M3.182.4` moved `boot.rs`'s prints into the `Append` sink and removed `boot.rs` from the allowlist, but the guard's own test still plants/fixtures the old expectation. A red test in the harness is a red branch.

# Fix

- Update the fixture to a path that is genuinely outside the allowlist **today** (not `boot.rs`), and keep asserting both directions: a planted print fails, the branch passes.
- Ensure the test does not depend on transient allowlist entries; if it enumerates the allowlist, derive it from `guard.py` rather than duplicating it.

# Acceptance

- `python -m unittest discover -s tools/tests` green; `tools/guard.py` OK on the branch; planted-print case still fails with a non-zero exit.
- `make check`; commit `[M3.187] ...`, done, no push.
