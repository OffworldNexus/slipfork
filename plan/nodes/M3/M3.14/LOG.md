# Log M3.14

## 2026-11-16T16:58 [planner]

created (task) Engine conformance + critical suites on BtrfsEngine (driver-injected fast suites, live kernel suite for the VM)

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

REQUIREMENT (M3.1 legality gate): engine conformance/critical suites must be deterministic at high host load; assert bounded teardown elapsed time rather than fixed-grace waits, and do not reintroduce fixed sleep windows.
