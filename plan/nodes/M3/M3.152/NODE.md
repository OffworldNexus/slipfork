---
id: M3.152
title: "Append-only boot output: completed rows as normal lines, one in-place status line, no cursor addressing or scroll regions"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.150]
paths:
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/boot_pty.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T09:43"
updated: "2026-11-25T11:12"
---

# Goal

Replace the absolute-addressed boot pane (DECSTBM `ESC[1;{rows-1}r`, `ESC[{row};1H`, block rows, park) with an append-only renderer so no width/geometry bug can ever clear or scramble the terminal again:

- Completed rows are ordinary newline-terminated lines in normal scrollback, in event order.
- At most one in-place status line at the bottom, redrawn with `\r` + `ESC[2K` only (carries the spinner, current step, elapsed, and the tail phase suffix M3.150 added).
- Streamed child output appended as normal lines with the `│`/`┃` gutter and indent rules; no row math.
- Exit writes nothing (no park, no clear, no margin reset). `SIGWINCH` only re-measures widths.
- `--json`/`NO_COLOR`/pipes: zero escapes, zero `\r`; unmeasurable terminal falls back to plain lines.

# Acceptance

- Byte-exact: no `ESC[<n>;1H`, `ESC[r`, `ESC[J`, `ESC c`, `?1049`, `?47`, `?1047`; only the status line uses `\r` + `ESC[2K`.
- Real PTY captures (110×30 and 80×24) of cached/fresh `up`, a failing build, and a resize mid-run: every row survives in scrollback, prompt on a fresh line.
- `make check`; Triad; commit `[M3.152] ...`, done, no push.
