---
id: M3.51.1
title: "DECSTBM flow-cursor prompt fix + nag removal + restart labels + 2D VT100 grid test"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/boot.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/daemon/lifecycle/core.rs
  - "crates/slipfork/tests/support/**"
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/critical_cli_surface.rs
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T17:24"
updated: "2026-11-22T17:37"
---

# Goal

Task rows must never paint over the shell prompt: track a real flow cursor inside the `DECSTBM` region, clear row `rows` on `restore()`, drop the `world keeps running` nag, label `restart` rows with real components, and prove it with a real 2D VT100 screen-grid test.

# Context

- `crates/slipfork/src/boot/pane.rs:120-133` — `println()` writes `\x1b[{};1H{text}\r\n` at hardcoded `rows - 1` with no `\x1b[2K` and no flow-cursor tracking, so `✓ image backend cached 2msipfork up` lands on the user's prompt.
  - Fix: query the cursor (crossterm `cursor::position()` / DSR `\x1b[6n`, safe fallback); if on row `rows`, emit `\r\n` first so the prompt scrolls up. Keep `ScrollRegionGuard::install` (`:195`) writing `\x1b[1;{rows-1}r`, add `flow_row` (seed from the cursor row, clamped `<= rows-1`), print each row as `\x1b[{flow_row};1H\r\x1b[2K{text}\r\n`, advancing `flow_row = (flow_row + 1).min(rows - 1)`.
  - Bottom bar strictly inside `\x1b7\x1b[{rows};1H\x1b[2K{panel}\x1b8`.
- `crates/slipfork/src/boot/pane.rs:152-166` — `restore()` homes to `rows`. It must clear row `rows`, write `\x1b[r`, then park at `\x1b[{flow_row};1H\x1b[2K` (one line below the last printed row). `set_size()` (`:110`) must reseed `flow_row` after resize.
- Delete the nag: `crates/slipfork/src/boot/render.rs:244-248` `hint_line()` emits `world keeps running; use \`slipfork down\` when finished` — remove the fn and every caller (`grep -rn hint_line crates/`).
- `restart` mislabel: `crates/slipfork/src/world/up.rs:159-200` `restart()` stream + summary use the world label, producing `✓ service <world> running`. Emit the daemon's real component rows (`service <svc>`, `world <name>`); keep `-s/--service <svc>` = one service vs no service = every service.
- Test support: add `crates/slipfork/tests/support/vt.rs`, a real `(rows × cols)` grid handling `\r`, `\n` (scroll only inside `\x1b[{top};{bottom}r`), `\x1b[r`, `\x1b[H`/`\x1b[{r};{c}H`, `\x1b[2K`, `\x1b[K`, `\x1b7`/`\x1b8`, wide chars via `console::measure_text_width`. Export `Screen::row_text()`; `tests/support/` already holds `daemon_reap.rs`, `reference_control.rs`.
- Pre-seed `slipfork@host:~/demo$ slipfork up\r\n` at BOTH `row = rows` and `row = 3`, feed real PTY output of `up`, `restart`, `down`, `fork`, assert the prompt text is byte-identical on its row and no row lost characters.
- Zero `\x1b[` under `NO_COLOR` / non-TTY / `--json` (existing ANSI matrix invariants).

# Acceptance

- `cargo fmt --all -- --check`
- `cargo clippy -p slipfork --all-targets -- -D warnings`
- `cargo test -p slipfork --test ansi_matrix` — new `vt_grid_*` cases: bottom-row and row-3 prompts intact across `up`, `restart`, `down`, `fork`
- `cargo test -p slipfork --test critical_cli_surface`
- `cargo test -p slipfork --test status_surface`
- `grep -rn "world keeps running" crates/` prints nothing
