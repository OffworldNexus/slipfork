# Log M3.19

## 2026-11-16T16:58 [planner]

created (task) Live verification on the Btrfs DO VM: BtrfsEngine + crun + postgres:18 suites, measured numbers, reboot-safe SSH

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

ADD (M3.1 legality gate): the live VM run must include one make check under deliberately elevated host load (e.g. parallel CPU burners) to prove the Btrfs engine stop path is bounded-but-abort-aware, not grace-window dependent.
