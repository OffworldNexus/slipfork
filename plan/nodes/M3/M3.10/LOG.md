# Log M3.10

## 2026-11-16T16:58 [planner]

created (task) Rewrite docs/design/do-vm-harness.md: Btrfs volume, crun, AppArmor userns, persistent --identity

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T18:59 [lead]

REQUIREMENT (from M3.7 legality review): the AppArmor userns profile section must state that the profile permits the world-init path to create user namespaces (i.e. it cannot simply deny userns globally) and name /usr/bin/unshare explicitly, matching what M3.16 provisions on the VM.
