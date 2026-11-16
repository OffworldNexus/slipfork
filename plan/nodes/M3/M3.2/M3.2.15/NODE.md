---
id: M3.2.15
title: "0.3.0 test-surface audit + conformance matrix + support registry promotion"
kind: task
status: ready
owner: "test-curator"
priority: 4
depends_on: [M3.2.13, M3.2.14]
paths: ["docs/design/m3-test-audit.md", support/integrations.toml, "docs/support-matrix.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Audit the `0.3.0` test surface and promote registry evidence: verify every adapter (pytest, `CommandTestAdapter`, socket runner) is covered by the shared conformance suite plus a protected critical test, record the matrix in `docs/design/m3-test-audit.md`, and sync `support/integrations.toml` + `docs/support-matrix.md` to the evidence that actually exists.

# Context

- `ADR-0007`'s tier ladder is evidence-checked (`SupportRegistry::required_evidence`), and `H-009` Option B forbids claiming tiers without a recipe page and critical test.
- New adapters must not enter the registry at a tier they cannot support; anything unshipped stays omitted.

# Design / Invariants

1. `docs/design/m3-test-audit.md`: one row per `TestAdapter`/`CoverageDriver` implementation and per `crates/slipfork-test` protected suite, naming the conformance-suite invocation, the critical test file/function, and the recipe page; flag any behaviour asserted only by unit tests.
2. Confirm by inspection + commands: no `#[ignore]` in `crates/slipfork-test/tests/`, `crates/slipfork-engine/tests/`, `crates/slipfork/tests/`; no adapter without `assert_test_adapter_conformance`; no coverage path without a failing-threshold test.
3. `support/integrations.toml`: add/raise entries only where the audit row has both a recipe page and a critical test (expected: `pytest-slipfork`, `slipfork-test-cli`, `typed-coverage`); tier = `declared` at most. Never reference a deleted engine.
4. Regenerate `docs/support-matrix.md` with `make support-sync`; never hand-edit. `make support-check` must show zero drift and all pointers resolving.
5. If an adapter lacks required evidence, keep it out of the registry and record it as an explicit follow-up in the audit doc — never inflate a tier.

# Acceptance

```bash
test -f docs/design/m3-test-audit.md
grep -n 'pytest-slipfork\|CommandTestAdapter\|SocketRunnerAdapter' docs/design/m3-test-audit.md
! grep -rn '#\[ignore' crates/slipfork-test/tests/ crates/slipfork-engine/tests/ crates/slipfork/tests/
make support-sync
make support-check
uv run --no-project tools/critical_guard.py
uv run --no-project tools/check_support.py --check
make docs-check
```

Expected: the audit exists and covers every adapter; the inverted `grep` prints nothing; `support-sync` then `support-check` exit 0 with the regenerated matrix; `critical_guard.py` reports no unprotected modification; `check_support.py --check` and `docs-check` exit 0.
