---
id: M3.50.4
title: "DECSTBM bottom bar + ScrollRegionGuard, 1 line per task, zero StepLog, orphaned-pod visibility & Forkfile-digest reconcile"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.50.2, M3.50.3, M3.50.9]
paths:
  - Cargo.toml
  - Cargo.lock
  - crates/slipfork/Cargo.toml
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/status/**"
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/view.rs
  - crates/slipfork/src/daemon/registry.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - crates/slipfork/src/daemon/lifecycle/up.rs
  - crates/slipfork/src/world/up.rs
  - "crates/slipfork-world/src/control/**"
  - "crates/slipfork-world/src/bin/world_init/control.rs"
  - "crates/slipfork-world/src/bin/world_init/respond.rs"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/critical_daemon.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T15:44"
---

# Goal
`up`/`restart`/`fork`/`down` render exactly one aligned line per finished task above a fixed 1-line `apt`-style panel on row `rows`, with zero `StepLog`/`sha256`/`dockerfile`/hook bytes ever reaching the terminal, truthful `cached` words, a crisp `✗ ... — check logs with: slipfork logs <id>` failure, and terminal state restored on success, error, panic, and `SIGINT`.

# Context
- Replace `indicatif::MultiProgress`: `crates/slipfork/src/boot/pane.rs` (`insert_after(&self.summary)`), `crates/slipfork/src/boot/render.rs`, `crates/slipfork/src/boot.rs` (module docs + `BootTui` drivers), `crates/slipfork/src/boot/ascii.rs`, `crates/slipfork/src/ui/paint.rs`. Drop `indicatif` from `crates/slipfork/Cargo.toml:36-39` (keep `console`, `crossterm`, `libc`) and refresh `Cargo.lock`.
- Frame contract (spec 02 §3): scroll region `\\x1b[1;{rows-1}r` only when `rows >= 3`; row `rows` = `<spinner> [<24-cell bar>]  <done>/<total>  <pct>% · <active summary> · <elapsed>`, `filled = round(done/total*24)`, `pct = round(done/total*100)`; clamp by dropping `· active summary` first, then the bar when `cols < 80`, never shearing counters; `rows < 3` -> no `DECSTBM` at all, in-place `\\r` line (never `\\x1b[1;1r` / `\\x1b[1;2r`).
- `ScrollRegionGuard` RAII: clears row `rows` and writes `\\x1b[r` on `Drop`, including error, panic unwind, and `SIGINT`; `SIGWINCH` re-queries `(cols, rows)`, re-applies the region, redraws row `rows`.
- Modes: `ColorTty` as above; `AsciiTty` (`NO_COLOR`/`TERM=dumb`) = `|/-\\` spinner + 24-cell `[#.]` bar, **zero `\\x1b[` bytes** and no SGR; `PlainPipe` = `→ [i/n] <kind> <name>` headers + `✓`/`•` rows with `0` `\\r` and `0` `\\x1b[`; `Json` = pure JSON. Keep all four branches in one cohesive `UiMode`/`BootTui` struct `<= 500` LOC per file.
- Suppression: `BootTui` ignores every `log`/`StepLog` event frame (they are appended to `<store>/logs/<world>/<id>.log` by M3.50.1/M3.50.3); the CLI never prints log frames.
- Truthful words come from `BootState::as_str` (`crates/slipfork-world/src/boot/schedule.rs:23-48`): render `• image <name> cached 1ms` on hits, never `pulled`/`built`.
- Composition: this slice owns `crates/slipfork/src/main.rs` (`UiMode` + `BootTui` + guard wiring); M3.50.2 deliberately stayed out of it.
- `down` rows: `✓ pod api stopped 40ms`, `✓ removed run subvolumes 312ms`, `✓ Stopped <world> (370ms)`; persistent Btrfs data subvolumes and `<store>/logs/<world>/` survive.

# Acceptance
- `cargo test -p slipfork --test critical_status_live` in a real PTY (`openpty`/`script -qec`): `\\x1b[1;{rows-1}r`, 24-cell bar math, counters intact at `cols < 80`, no `DECSTBM` at `rows < 3`, one line per task, zero `sealed sha256:`/`dockerfile` bytes, `cached` on hits, `✗ Failed to build image <id> — check logs with: slipfork logs <id>` plus `\\x1b[r` on failure, clean `SIGWINCH` redraw.
- Same PTY with `NO_COLOR=1` and `TERM=dumb`: `0` `\\x1b[` bytes, no SGR, `\\r`-rewritten ASCII line.
- `cargo test -p slipfork --test ansi_matrix`, `--test status_surface`, `cargo test -p slipfork`, `cargo clippy --workspace --all-targets -- -D warnings`, `uv run --no-project tools/guard.py`.
