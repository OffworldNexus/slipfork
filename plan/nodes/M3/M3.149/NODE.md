---
id: M3.149
title: "up resets the terminal at exit: ScrollRegionGuard::drop moves the cursor to the last row, erases it and resets margins after the final output"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/boot/**", crates/slipfork/tests/ansi_matrix.rs]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T08:57"
updated: "2026-11-25T09:43"
---

# Owner report

"the terminal gets reset at the end of `slipfork up`… what the fuck? why you keep on breaking this?"

# Cause (code)

`crates/slipfork/src/boot/region.rs:101-110` — `ScrollRegionGuard::drop` always writes:

```
ESC[<rows>;1H   move cursor to the terminal's last row
ESC[2K          erase that whole line
ESC[r           reset the scroll margins
```

This runs **after** the pane's final rows (including the `✓ … Ready` verdict) are painted, so it yanks the cursor to the bottom, erases whatever line lives there (often the shell prompt or the last log line the user wanted to keep), then resets margins — which reads as "the terminal got reset". `set_size` (`pane.rs:105-110`) takes the guard through the same `Drop` on every `SIGWINCH`, so a resize also erases a line mid-run.

# Fix

- Exit restoration is **margins only**: `ESC[r`. No cursor move, no `ESC[2K`, no clearing, on the normal and resize paths. The SIGINT path may keep its erase (the process is dying and the cursor sits mid-pane).
- `set_size` must not run the destructive exit sequence: reset margins and re-install without moving/erasing (or make the guard's resize transition a separate non-destructive call).
- Install/drop only when a region was actually installed (`rows >= 3`) and stdout is the interactive terminal; plain/`--json`/piped runs must emit none of these sequences.
- Audit the rest of the exit path (`pane` drop order) for any other sequence written after the final row.

# Acceptance

- Byte-exact test: the guard's normal exit emits `ESC[r` and **no** `ESC[<n>;1H`, `ESC[2K`, `ESC[J`, `ESC c`, or alt-screen sequence; resize transition likewise.
- PTY capture of a real `up` (110×30): all rows and the `Ready` line remain visible after exit, no trailing blank/erased line, `NO_COLOR` unchanged, `--json` has zero escapes.
- `make check`; Triad; commit `[M3.149] ...`, done, no push.
