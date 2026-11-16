# Log M3.8

## 2026-11-16T16:58 [planner]

created (task) Rewrite docs/design/world-base-image.md on unified Btrfs subvolumes

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T19:19 [lead]

STALE VOCABULARY (M3.7 cruft sign-off): docs/design/world-base-image.md:364 still carries TestCoordinator/RunnerProtocol v1 wording; the rewrite must drop the dead protocol names and describe only the Btrfs/crun substrate plus the STP v1 pointer (Phase 2 M3.2).
