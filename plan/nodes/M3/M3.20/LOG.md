# Log M3.20

## 2026-11-16T17:25 [strategist]

created (task) Workspace trait & test decoupling: drop SnapshotMode/StatefulRam and replace FakeIncus in slipfork-edge

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T21:44 [lead]

REQUIREMENT (M3.9 cruft gate): crates/slipfork-edge/src/daemon.rs:82 documents ResourceSummary.id as pg_databases.main while diagnostics() emits bare ids; align the doc comment with the shipped bare-id contract.
