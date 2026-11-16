# Log M3.9

## 2026-11-16T16:58 [planner]

created (task) Rewrite docs/design/target-dx.md for the Btrfs-only world (doctor, world entry, zero incus)

## 2026-11-16T16:58 [planner]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:32 [lead]

OWNER TRUTHFULNESS CONSTRAINT: keep target-dx.md free of any promise of CommandTestAdapter, packages/pytest-slipfork, Rust-side CoverageDriver, runner.sock NDJSON adapters, or published test plugins. Phase 2 is STP v1 (inherited SLIPFORK_PROTOCOL_FD socketpair, framework-exposed DAG stage nodes incl. persistent fixture stages frozen to RO Btrfs snapshots, coverage+mutants in-protocol, language-blind slipfork-test orchestrator, ad-hoc pytest/vitest/phpunit proofs). Describe only what Phase 1 ships; defer STP v1 prose to the rewritten Phase 2 docs.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:48 [lead]

status ready -> active

## 2026-11-16T20:48 [lead]

attempt 1/3 started

## 2026-11-16T20:48 [lead]

base 4d5b02efd7444c3413a6d238271c2a700ed9fd5a

## 2026-11-16T20:58 [lead]

status active -> review

## 2026-11-16T21:00 [lead]

edited fields

## 2026-11-16T21:01 [lead]

QUAD GATE attempt 1 (legality FAIL, cruft FAIL, ioc FAIL/SPEC_BLIND_SPOT escalated to M3.25): invented [resources] <kind>.<id> prefixes (shipped renderer emits [resources] {id}:), fabricated netns uid parenthetical, filesystem name vs fstype i64 (ownership filed on M3.15), dual world-id syntax in --json vs console (escalated), canonical naming residue, quadruplicated crun exec rule, --world selection rule has two homes.

## 2026-11-16T21:35 [lead]

status review -> active

## 2026-11-16T21:35 [lead]

attempt 2/3 started

## 2026-11-16T21:35 [lead]

base 7cf08b5202c58593b3f6c4031bebf200a8e24f97 (attempt 2)

## 2026-11-16T21:43 [lead]

status active -> review

## 2026-11-16T21:45 [lead]

IOC GATE attempt 2 PASS (minor: fstype label table duplicates ADR-0010 §4 -> keep pointer; name both owners for <state_dir>/<project>/worlds/<world_id>/logs (Layout::world_logs_dir) vs <state_dir>/worlds/<world_id> (BtrfsEngine); M3.16 NODE body corrected).

## 2026-11-16T21:45 [lead]

LEGALITY GATE attempt 2 PASS (minors): auth p256 key abbreviated with an ellipsis is not reproducible output; `slipfork test [-j N]` needs an owner (M3.2) or must drop the flag; logs path must cite Layout::world_logs_dir not ADR-0010 §2; drop <project>:<fork> bullet needs its M3.15 target-state label.
