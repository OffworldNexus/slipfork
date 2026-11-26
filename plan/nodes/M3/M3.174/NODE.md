---
id: M3.174
title: "Slice 2c/S1: Pane mechanics on ui/report/live.rs (byte-identical boot pane)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/boot.rs
  - crates/slipfork/src/boot/status.rs
  - "crates/slipfork/src/boot/status/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T17:25"
updated: "2026-11-25T17:35"
---

# Goal

First step of the Report boot-pane migration: move the pane's byte-identical mechanics into `ui/report/live.rs` as `Pane` (the only writer of `\r\x1b[2K`, `O(active+8)` rows), wired from `boot.rs`/`boot/status*`. No row semantics change yet (S2/S3 follow), so every existing boot/PTY test must stay byte-identical.

# Acceptance

- HEAD-vs-after PTY transcripts (110×30 and 80×24: cached, fresh, failing, streaming) are byte-identical.
- `boot_pty` 4/4, `ansi_matrix` green; every touched file ≤500 LOC; no `Palette` re-derivation.
- `make check` on the pinned toolchain (`CARGO_TARGET_DIR=/var/tmp/m3174-target CARGO_PROFILE_DEV_DEBUG=0`, delete after); Triad-light (`review-tests`); commit `[M3.174] ...`, done, no push.
