---
id: M3.145
title: "Report core: ui/report/ model + renderer, main/dispatch/handoff, WallZone"
kind: task
status: done
owner: implementer
priority: 1
depends_on: []
paths:
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/ui/mod.rs
  - crates/slipfork/src/ui/palette.rs
  - crates/slipfork/src/ui/failure.rs
  - crates/slipfork/src/ui/paint.rs
  - crates/slipfork/src/ui/table.rs
  - crates/slipfork/src/ui/terminal.rs
  - crates/slipfork/src/compose.rs
  - crates/slipfork/src/commands/timeparse.rs
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/commands/runs.rs
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/commands/streams/render.rs
  - crates/slipfork/src/commands/exec.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/cli/dispatch.rs
  - crates/slipfork/src/daemon/handoff.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/tests/ansi_matrix.rs
  - Cargo.toml
  - Cargo.lock
  - crates/slipfork/Cargo.toml
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T03:42"
updated: "2026-11-25T03:57"
---

# Goal
Slice 1 of `specs/02-cli-ux.md` §9.5: land the typed `ui/report/` core (`Report`, `Section`, `Entry`, `Row`, `RowLevel`, `Outcome`, `Done`, `StepSettlement`, `FailureFacts`, `Failure`, `WallZone`, `Terminal { cols, rows, mode, stdin_tty }`, `ReportRenderer`) and route `main.rs`, `cli/dispatch.rs`, `daemon/handoff.rs` and `commands/timeparse.rs` through it. Delete `unsafe` `tzset`/`localtime_r` and every raw print in those three files.

# Context
- Authoritative scope: `specs/02-cli-ux.md` §9.1–§9.5 (Slice 1 row) and §9.2 (renderer owns `Palette`, `UiMode`, ANSI-stripped `Table`, stream gutters, blank-line rules).
- New dir `crates/slipfork/src/ui/report/`: `mod.rs` = composition only; `row.rs` (Row/Outcome/Done), `render.rs` (ReportRenderer), `wall.rs` (WallZone), `failure.rs` (Failure + stderr/`--json` projection). Each <=500 LOC.
- Migrate/delete sites (all `crates/slipfork/src`): `main.rs:40,53,79,99` (`Palette::new` + `ui::failure_block`), `main.rs:42,55,81,100` (`eprintln!`), `main.rs:89` (`println!` ingress listening), `main.rs:109` (`LocalClock::resolve`); `cli/dispatch.rs:219,247,253,267` (`println!`) and `:273,274,301,302` (`eprintln!`); `daemon/handoff.rs:79,80,110,111,155,156` (`eprintln!` error/next pairs → one `Failure`).
- `commands/timeparse.rs`: delete `unsafe tzset` (`:9`, `:208`) and `localtime_r` (`:260-265`); `LocalClock` (`:188-203`) becomes a thin `WallZone` delegate (deleted in the last slice). Add a hand-rolled RFC 8536 TZif reader `WallZone::from_tzif(bytes)` — **no new crate, do not edit `Cargo.toml`/`Cargo.lock`**; if a manifest change is unavoidable, stop with `STATUS: blocked` (`missing-input`).
- `ui/{palette,failure,paint,table,terminal}.rs` stay `pub(crate)`; `ui/terminal.rs` (27 LOC) folds into the renderer's `Terminal`; `compose.rs` is the single `SIGWINCH` → `Resize { cols, rows }` forwarding site (§9.2) — confirm it exists first; `ui/table.rs:176` (test uses `crate::ui::paint`) stays valid because `paint` is still `pub(crate)`.
- Do **not** touch `crates/slipfork/tests/critical_*.rs`; now touching `daemon/reconcile`, `daemon/client.rs`, `boot/**`, `commands/**` or `world/**`.

# Acceptance
- `Row` has no `icon`/`state: String`; `Done::built_or_cached(0) == Outcome::Cached`, `n>=1 == Outcome::Ok(Done::Built { steps: NonZeroUsize })`; `Skipped` never prints `#run_id`.
- `grep -n 'eprintln!\|println!\|Palette::' crates/slipfork/src/main.rs crates/slipfork/src/cli/dispatch.rs crates/slipfork/src/daemon/handoff.rs` → no matches; `grep -n 'tzset\|localtime_r\|unsafe' crates/slipfork/src/commands/timeparse.rs` → no matches.
- Byte-exact/PTY proof: PTY journey runs `main --listen` failure and asserts (a) zero `\x1b[` with `NO_COLOR=1`, (b) exactly one `next:` line, (c) stdout/stderr bytes equal the `--json` `{"error":{code,message,next}}` projection.
- `WallZone::from_tzif` unit test pinned against fixed epoch integers across a CET/CEST DST boundary (safe parallel Rust).
- `cargo test -p slipfork` and `cargo clippy --workspace --all-targets -- -D warnings` clean.
