# Log M3.18

## 2026-11-16T16:58 [planner]

created (task) Published docs align with the shipped Btrfs-only surfaces (README, index, getting-started, CLI reference, AGENTS.md)

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:32 [lead]

OWNER TRUTHFULNESS CONSTRAINT: do not publish CommandTestAdapter, pytest-slipfork, per-language CoverageDriver, or any test-plugin package/CLI invocation. Phase 2 is STP v1 and is being rewritten; describe only shipped Btrfs/doctor/daemon surfaces.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.
