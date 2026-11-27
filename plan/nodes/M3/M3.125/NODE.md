---
id: M3.125
title: "Delete Palette::dim alias: one metadata role (meta), migrate every caller"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/ui/palette.rs
  - "crates/slipfork/src/doctor/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/daemon/client.rs
  - "crates/slipfork/src/boot/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T19:29"
updated: "2026-11-24T19:29"
---

# Problem

`M3.124` introduced the neutral `38;5;244` metadata role but kept `Palette::dim` as a thin alias so doctor/env/secret/docker didn't need edits. Owner rule: no leftover spaghetti, one role per semantic, no compatibility aliases.

# Change

- Delete `Palette::dim` (and any `self.dim` wrapper). Rename every caller to the metadata method (`meta`, or `muted`—pick one and use it everywhere).
- No behaviour change: the replaced calls must still render `38;5;244` (or plain under `NO_COLOR`); counters/timings/timestamps/`next:` label/stdout bar/notes stay metadata grey.
- Grep gate: no `palette.dim(`/`self.dim(`/`fn dim(` left.
- `make check`; commit `[M3.125] ...`, done, no push.
