# Log M3.12

## 2026-11-16T16:58 [planner]

created (task) SubvolumeDriver seam: KernelBtrfsDriver (statfs + BTRFS_IOC_*) and test-only tree driver

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T21:45 [lead]

M3.25 RULING: implement EngineError::SubvolumeDestroyDenied { path, errno, mount_hint } for the destroy-stage EPERM path alongside BtrfsRequired and BtrfsProbeReport (ADR-0010 §4).
