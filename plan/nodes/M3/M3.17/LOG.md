# Log M3.17

## 2026-11-16T16:58 [planner]

created (task) poc benchmark + support registry on the Btrfs engine (fails closed off-Btrfs)

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-17T12:17 [lead]

status ready -> active

## 2026-11-17T12:17 [lead]

attempt 1/3 started

## 2026-11-17T12:17 [lead]

base 053596ee33193d5e947e933bf2c0506f366ff3f8

## 2026-11-17T12:29 [lead]

Quad gate 1/2: ioc BLOCKER (ctypes/statfs re-implements the shipped doctor seam; duplicate BTRFS_MOUNT_HINT makes the hint assertion drift-blind) + cruft MAJOR (header lists doctor; bench-baseline topology/note pin M1 provenance that refresh_baseline never rewrites) → attempt 2: probe through 'slipfork doctor --json', rename snapshot_ro_ms→first_up_ms, fix baseline provenance and test hygiene.

## 2026-11-17T12:34 [lead]

edited fields
