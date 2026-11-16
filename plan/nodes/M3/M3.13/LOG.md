# Log M3.13

## 2026-11-16T16:58 [planner]

created (task) BtrfsEngine: sole production WorldEngine (unified per-world subvolume, RO snapshots, writable clones, --ipc); delete IncusEngine, ReflinkNetnsEngine and FICLONE

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

REQUIREMENT (M3.1 legality gate): the engine stop path must be bounded-but-abort-aware — do not carry a fixed grace (the current 10s WORLD_STOP_GRACE red-lines world-booting suites under host load ~40). SIGTERM, then poll for exit within a bound, escalate to SIGKILL only after the bound, and report the elapsed time so suites can assert real teardown instead of waiting a fixed window.
