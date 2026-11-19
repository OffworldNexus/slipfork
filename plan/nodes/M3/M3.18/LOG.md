# Log M3.18

## 2026-11-16T16:58 [planner]

created (task) Published docs align with the shipped Btrfs-only surfaces (README, index, getting-started, CLI reference, AGENTS.md)

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:32 [lead]

OWNER TRUTHFULNESS CONSTRAINT: do not publish CommandTestAdapter, pytest-slipfork, per-language CoverageDriver, or any test-plugin package/CLI invocation. Phase 2 is STP v1 and is being rewritten; describe only shipped Btrfs/doctor/daemon surfaces.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T21:01 [lead]

edited fields

## 2026-11-16T21:01 [lead]

PATH WIDENING: docs/testing-dag.md is a published page still describing RunnerProtocol v1 / CommandTestAdapter / per-language CoverageDriver (flagged twice by reviewers). Rewrite it as the STP v1 (Phase 2 M3.2) pointer plus the shipped [artifacts]/[fixtures] DAG surface, with zero promise of per-language Rust drivers or published plugins.

## 2026-11-16T21:52 [lead]

OWNER DIRECTIVE (CBOR): docs/testing-dag.md must state the STP v1 wire is CBOR (RFC 8949) over the inherited SLIPFORK_PROTOCOL_FD socketpair — never NDJSON/JSON — as Phase-2 M3.2 target state.

## 2026-11-16T23:51 [lead]

OWNER REGROUP RULING (binding): shipped version is 0.1.0 on release/0.1.0, NOT 0.3.0/0.2.0. Every published page you touch (README.md, docs/index.md, docs/getting-started.md, docs/reference/cli.md, AGENTS.md, docs/testing-dag.md) must say 0.1.0 and describe the shipped Btrfs-only surface (doctor Btrfs probe + live subvol round-trip, Btrfs host prerequisite, crun inside the L0 rootfs). Zero Incus/ZFS/FICLONE claims, zero vaporware.

## 2026-11-17T10:36 [lead]

status ready -> active

## 2026-11-17T10:36 [lead]

attempt 1/3 started

## 2026-11-17T10:36 [lead]

base a602f1e5a66f6b3800dbce14894546611075fb2a

## 2026-11-17T10:56 [lead]

attempt 2/3 started

## 2026-11-17T10:56 [lead]

attempt 1 hit the step limit after rewriting README/index/getting-started/cli/AGENTS. Attempt 2 finishes testing-dag, runs the gates + critics, writes RESULT.

## 2026-11-17T12:45 [lead]

Gate: docs critics (dx, ai-ergonomics, cruft) applied in RESULT; acceptance greps + make docs-check exit 0; --world placeholder aligned to <WORLD> after M3.15 shipped the clap value_name.

## 2026-11-17T12:45 [lead]

status active -> review

## 2026-11-17T12:45 [lead]

status review -> done

## 2026-11-17T13:09 [lead]

FINAL: functional PASS, cruft findings fixed in-path (AGENTS 0.1.0 split, doctor fix-per-check, sudo/AppArmor required, --repo undocumented+deleted, FsDigestCache ship status, README dedupe); out-of-path version/sibling-page items on M3.23. docs-check/guard/plan green.

## 2026-11-17T13:09 [lead]

status done -> done
