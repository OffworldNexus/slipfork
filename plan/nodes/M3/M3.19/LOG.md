# Log M3.19

## 2026-11-16T16:58 [planner]

created (task) Live verification on the Btrfs DO VM: BtrfsEngine + crun + postgres:18 suites, measured numbers, reboot-safe SSH

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

ADD (M3.1 legality gate): the live VM run must include one make check under deliberately elevated host load (e.g. parallel CPU burners) to prove the Btrfs engine stop path is bounded-but-abort-aware, not grace-window dependent.

## 2026-11-16T20:59 [lead]

REQUIREMENT (M3.8): run `slipfork prune` before asserting an empty `btrfs subvolume list` — the digest-keyed sealed base subvolume survives until GC (world-base-image.md §7.2 #4 / §8.4 #5).

## 2026-11-16T21:03 [lead]

ACCEPTANCE REFINED: teardown asserts `slipfork prune --yes` then `btrfs subvolume list | grep -v /snapshots/` empty — the digest-keyed sealed base under <state_dir>/snapshots survives until the Phase-2 M3.2.13 GC, so a globally empty list is not assertable in Phase 1.

## 2026-11-16T21:34 [lead]

M3.25 RULING APPLIED: host crun --version replaced by in-world slipfork shell -- crun --version.

## 2026-11-16T21:44 [lead]

ACCEPTANCE ORDER: `slipfork up` precedes the in-world `slipfork shell -- crun --version` check (a world must exist for in-world verification).
