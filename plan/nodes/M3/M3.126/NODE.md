---
id: M3.126
title: "Delete Palette::dim alias: one metadata role (meta), migrate every caller"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/ui/palette.rs
  - "crates/slipfork/src/doctor/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/daemon/client.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/world/runtime/frames.rs
  - "crates/slipfork-image/src/step_log.rs"
  - "crates/slipfork-image/src/builder/mod.rs"
  - "crates/slipfork-image/src/lib.rs"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T19:29"
updated: "2026-11-24T19:43"
---

# Goal

`M3.124` introduced the neutral `38;5;244` metadata role but kept `Palette::dim` as a thin alias so doctor/env/secret/docker didn't need edits. No leftover spaghetti: one role per semantic, no compatibility aliases.

# Change

- Delete `Palette::dim` (and any `self.dim` wrapper). Rename every caller to the single metadata method (`meta`).
- No behaviour change: replaced calls still emit `38;5;244` (plain under `NO_COLOR`).
- Grep gate: no `palette.dim(`/`self.dim(`/`fn dim(`.

# Acceptance

- `make check` green; byte-exact palette tests still pass; grep gate clean.
- Commit `[M3.126] ...`, done, no push.
