---
id: M3.127
title: Align critical_world_journey with the shipped status/logs schema
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [crates/slipfork/tests/critical_world_journey.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T20:02"
updated: "2026-11-24T20:08"
---

# Problem

`critical_world_journey` fails on the branch after M3.124/M3.126: the status row now has separate state/command columns (`service web running <command>`) and the replay gutter follows the actual stream (`│` stdout / `┃` stderr). The test still expects the old command-last status and the old gutter. This is a test/contract mismatch, not a renderer bug — align the assertions with `specs/02` and the M3.124 visual contract; do not weaken them.

# Acceptance

- `critical_world_journey` green on a real Btrfs VM (private tree, no `do_vm.py sync`, never `~/demo`).
- Assertions still check: exactly one remedy, exit code, command presence, and the correct stream glyph per line.
- `make check`; commit `[M3.127] ...`, done, no push.
