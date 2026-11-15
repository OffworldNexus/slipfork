---
id: M1.9
title: "Delete rejected [test] shell field, land typed coverage schema, per_world fixtures & support registry"
kind: task
status: ready
owner: implementer
priority: 1
depends_on: [M1.8]
paths:
  - "crates/slipfork-dag/**"
  - crates/slipfork/tests/critical_slug.rs
  - "support/**"
  - ARCHITECTURE.md
  - "docs/decisions/ADR-0006*"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T11:11"
updated: "2026-11-15T11:11"
---

# Goal

Back the `M1.8` disclosure with real code: the `0.1.0` `slipfork_dag::TestSpec` still carries a dead, never-executed field for the rejected post-suite shell surface that `ADR-0006` says never ships. Delete it and land the typed `[test]` coverage schema plus the support registry file, so the design of record and the crate agree before `0.1.0` merges.

**Release blocker for `0.1.0`.** Created by `lead` from the `M1.8` Quad Review Gate (see `plan/nodes/M1/M1.8/FOLLOWUPS.md` item MAJOR-1). Normative targets are already written: `ADR-0006` §5 (typed coverage + the "no stringly plumbing" law), `ADR-0003` §1 (`per_world`), `ADR-0007` (registry schema).

## Tasks

1. `crates/slipfork-dag/src/lib.rs`: delete the dead `TestSpec` string field. Add typed `TestSpec { coverage: Option<CoverageConfig> }` with `CoverageConfig { report: Vec<CoverageReportFormat>, fail_under: Option<CoverageThreshold> }`, `CoverageThreshold` (`0..=100`, fail-closed `DagError::InvalidCoverageThreshold`), all `#[serde(deny_unknown_fields)]`. No alias, no deprecated field, zero compatibility promise (Iron Law #3).
2. `FixtureSpec`: add `per_world: bool` (default `false`) per `ADR-0003` §1; document that `per_world = true` fixtures write absolute URLs and can never be shared across worlds.
3. Update the protected critical tests that pin the removed field (`crates/slipfork-dag/tests/critical_dag.rs`, `crates/slipfork/tests/critical_slug.rs`) — this node's `paths` authorize exactly those edits; do not weaken unrelated assertions.
4. Add `support/integrations.toml` with the initial entries and typed `evidence` fields as specified in `ADR-0007` (no ad-hoc prose matrix).
5. Remove the now-false "dead field, deleted later" disclosure from `ARCHITECTURE.md` and `ADR-0006` (the field no longer exists).

## Acceptance

- [ ] `rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR" crates support ARCHITECTURE.md docs CHANGELOG.md` exits 1 (0 hits).
- [ ] `uv run --no-project --with pytest pytest -q tools/tests` exits 0 (plan/tooling checks unaffected).
- [ ] `cargo test -p slipfork-dag` exits 0, including a critical test proving `coverage_fail_under = 101` fails closed with `InvalidCoverageThreshold` and that `coverage` cannot be set without a report format.
- [ ] `cargo test -p slipfork -p slipfork-dag` exits 0 after the `critical_slug.rs` / `critical_dag.rs` updates.
- [ ] `test -f support/integrations.toml` exits 0 and it parses through the typed `IntegrationEntry` schema.
- [ ] `make check` exits 0 and `uv run --no-project tools/critical_guard.py --base <base> --node M1.9` reports only the authorized protected-test edits.

## Notes

- Do not invent `weaved` or OSPP wire details; `H-006` stays deferred and OSPP is not a prerequisite for M2 (`H-008`).
- The registry file lands here; `slipfork support` + the `make check` ceiling land in M2 (`ADR-0007`).
