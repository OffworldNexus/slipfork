---
id: M3.151
title: "Replace the absolute-addressed boot pane with an append-only live log and one in-place status line (no scroll regions, no clears)"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/boot/**", "crates/slipfork/src/ui/report/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T09:33"
updated: "2026-11-25T09:43"
---

# Owner report (after M3.149)

"the screen still gets reset" — clarified: **the output disappears and the shell prompt ends up at the top of a cleared screen**, with the binary that includes `M3.149`.

# Why the current model cannot be trusted

The live pane paints with DECSTBM (`ESC[1;{rows-1}r`), absolute row addressing (`ESC[{row};1H`), in-place line erases and a final "park" — a full-screen cursor-addressed widget. Any wrong `rows` probe, resize race, terminal quirk (kitty/alacritty/tmux differ), or a write after the screen was scrolled produces exactly what the owner sees. M3.149 removed the erase-on-exit bytes and the screen still clears, so the model itself is the defect.

# New contract (one owner, no exceptions)

Append-only rendering, like `cargo`/`npm`:

- **Completed rows are printed as ordinary lines**, in order, one per event, into normal scrollback (`\n`). Never move the cursor above the current line; never address rows; never use DECSTBM, `ESC[H`, `ESC[J`, `ESC c`, alt-screen, or scroll regions.
- **At most one in-place status line** (the running step + spinner + elapsed) at the bottom, redrawn with `\r` + `ESC[2K` only. When the step completes, write its final row as a normal line and clear the status line.
- **Streamed child output** is appended as normal lines with the existing `│`/`┃` gutter and indent rules — no region math.
- **Exit writes nothing**: no park, no clear, no margin reset (there are no margins). The last thing on screen is the verdict row and a normal newline.
- Widths stay dynamic (strip ANSI when measuring), inner Dockerfile keywords stay lowercase muted magenta, `✓`/`✗`/`•`/`○` stay outcome-derived, `--json`/`NO_COLOR`/pipes emit zero escapes and no `\r`.
- Resize (`SIGWINCH`) only re-measures widths; it must not redraw history or clear anything.
- If the terminal cannot be measured, fall back to plain lines (no status line) — never guess a height.

# Acceptance

- Byte-exact tests: a full run's stream contains no `ESC[<n>;1H`, `ESC[r`, `ESC[J`, `ESC c`, `?1049`, `?47`, `?1047`; completed rows are newline-terminated lines; only the status line uses `\r` + `ESC[2K`; `NO_COLOR`/`--json`/pipe have zero escapes and zero `\r`.
- Real PTY captures (110×30 and 80×24) of `up` (cached and fresh), a failing build, and a resize mid-run: all rows remain in scrollback after exit, prompt appears on a fresh line, nothing cleared.
- `make check`; Triad (ux must verify scrollback preservation); commit `[M3.151] ...`, done, no push.
