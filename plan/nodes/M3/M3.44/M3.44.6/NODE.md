---
id: M3.44.6
title: "Frozen-bottom-pane indicatif MultiProgress TUI with PTY/NO_COLOR/TERM=dumb/--json gates"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.44.5]
paths:
  - Cargo.toml
  - Cargo.lock
  - crates/slipfork/Cargo.toml
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - "packages/**"
  - "crates/slipfork-world/src/**"
attempts: 6
max_attempts: 6
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T22:36"
---

# Goal
Build the frozen-bottom-pane live TUI on `indicatif::MultiProgress` + `console` + `owo-colors` for `up`/`restart`/`fork`/`down`/`doctor`, with aligned scrollback completions and strict TTY / `NO_COLOR` / `TERM=dumb` / non-TTY / `--json` behavior.

# Context
- New cohesive modules `crates/slipfork/src/cli/tui/{mod.rs,pane.rs,render.rs}` (each <= 300 LOC): `BootTui` owns one `MultiProgress`, the anchored 16-cell bar `[████████░░░░░░░░] done/total`, one per-node line with Braille spinner + live timer + `<= 3`-line dimmed `RUN`/hook tail, and `mp.println` for completed rows aligned as `8ch kind + 2sp + name + node_start.elapsed()` using dynamic widths from `crates/slipfork/src/status/widths.rs` (ANSI-stripped measurement; never `{:<16}` shearing).
- Wire in `crates/slipfork/src/daemon/lifecycle/stream.rs` (+ `{up,down,fork}.rs` as needed) and `crates/slipfork/src/world/up.rs` (already split to <= 300 + `world/up/nodes.rs` in M3.44.4); `crates/slipfork/src/render.rs` delegates to `cli::tui`; `crates/slipfork/src/status/{model,rows,tree,widths,mod}.rs` supply popped rows and `down`/`doctor` summaries.
- Gates (specs/02-cli-ux.md:169-172): TTY + color → Braille `⠋⠙⠹…`, `owo-colors` styling (`running` green, `failed` red, 12-char digests dimmed), dimmed tail; TTY + `NO_COLOR`/`TERM=dumb` → plain ASCII `|/-\` spinner and `[#.]` bar with **zero SGR and zero `\x1b[`**; non-TTY or `--json` → `ProgressDrawTarget::hidden()` plus `→ [i/n]` phase headers and `eprintln!` replay of `✓ <kind> <name> <state>  <dur>`, **zero `\r` and zero `\x1b[`**.
- Never block: all child work uses `spawn()` + streamed lines (no `.output()`, no bare `sleep`). All verbs stream — `down` emits per-pod stop rows + `removed run subvolumes`, `doctor` leads `✓ Ready`/`✗ N issue(s)`, `restart` dividers survive.
- Failure: clear pane, `✗ <kind> <name>`, `Forkfile:<line>`, `error: ...`, exactly one `next:` remedy, and surviving pods stay registered so `logs`/`down` work immediately.
- Deps (cached): `indicatif 0.17.11`, `console 0.15.11`, `owo-colors 4.4`, `crossterm 0.29` in root `Cargo.toml`.
- `<= 500 LOC`: split `daemon/lifecycle/stream.rs` into pane sink + plain sink modules rather than exceeding; no `cli/**` file > 300; `world/up.rs` stays <= 300.
- Out of scope: event production semantics (M3.44.4) and the `clap` surface (M3.44.5) — consume both unchanged. Zero mocks; PTY tests drive the real binary.

# Acceptance
- `cargo test -p slipfork --test critical_status_live` green in a real PTY (`script -qec`/`openpty`): Braille spinners, 16-cell bar, aligned `✓` rows with elapsed durations, dimmed 1–3 line tails; re-run `up` and `restart` in the same PTY with `NO_COLOR=1` and `TERM=dumb` → plain-ASCII pane, **0 `\x1b[` bytes**, no SGR.
- `cargo test -p slipfork --test critical_cli_surface --test ansi_matrix --test status_surface` green (non-TTY `→ [i/n]` replay, hidden draw target, 0 `\r`/0 `\x1b[`).
- `cargo test -p slipfork --test critical_world_journey` green (`down`/`restart` streaming rows, surviving pods registered).
- Final pre-1.0 purge gate: `rg -c 'seaweedfs|weed |static_cdn' crates docs README.md editors` → no matches.
- `cargo clippy --workspace --all-targets -- -D warnings` and `uv run --no-project tools/guard.py` exit 0.
