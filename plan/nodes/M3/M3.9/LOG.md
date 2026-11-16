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
