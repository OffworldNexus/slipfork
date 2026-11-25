---
id: M3.147
title: "Docker/streams/runs/exec on ReportRenderer: inner steps, Stream rows, 12-char digests"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.141, M3.146]
paths:
  - "crates/slipfork/src/ui/report/**"
  - "crates/slipfork/src/commands/docker/**"
  - crates/slipfork/src/commands/streams.rs
  - "crates/slipfork/src/commands/streams/**"
  - crates/slipfork/src/commands/runs.rs
  - crates/slipfork/src/commands/exec.rs
  - crates/slipfork/tests/docker_surface.rs
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-25T03:42"
updated: "2026-11-25T18:23"
---

# Goal
Slice 3 of `specs/02-cli-ux.md` §9.5: `docker build/pull`, `logs`, `logs -r`, `list`/`images`/`--runs`, and `exec` render through `ReportRenderer` inner-step + `Entry::Stream` rows and 12-char digest tables; refusals become `Outcome::Refused` + one `Failure`.

# Context
- Depends on `M3.141` (`commands/**` owner) and `M3.146` (`ui/report/live.rs`); serialized so only one writer touches `ui/report/mod.rs`.
- Deletions: `commands/docker/printer.rs` (254 LOC, `EventPrinter`), `commands/streams/render.rs` (319 LOC), the SGR-`2` `paint` usage in `commands/runs.rs` (`:11`), raw `eprintln!` in `commands/exec.rs:191`.
- Migrate anchors: `commands/docker/build.rs:90`; `commands/docker/registry.rs:104,138,171,270` + stray `eprintln!()` at `:306`; `commands/docker/printer.rs:231` (`keyword_cell`); `commands/streams.rs:144` (`eprintln!` notice); `commands/streams/render.rs:65,307,315`.
- Column widths latch once from `plan`/`planned_instruction_width`/`roster` (§9.4) — no hardcoded `{:<12}`; digest cells are 12 chars dimmed.
- `Stream { indent, stream, line, ts_ms }` flushes once and drops; `BrokenPipe` exits `0`. Do not swallow slow child output — spawn + stream `[step/total]` headers.
- Do **not** touch `crates/slipfork/tests/critical_*.rs`.

# Acceptance
- `ls crates/slipfork/src/commands/docker/printer.rs crates/slipfork/src/commands/streams/render.rs` → both gone; `grep -rn 'EventPrinter\|ui::paint\|eprintln!' crates/slipfork/src/commands/docker crates/slipfork/src/commands/streams* crates/slipfork/src/commands/runs.rs crates/slipfork/src/commands/exec.rs` → no matches.
- PTY proof: `docker build` shows `[i/n]`/`n/N` inner steps with live streaming; `logs -r` replay rows carry `HH:MM:SS.mmm` under `TZ=Europe/Paris`; `images`/`list`/`--runs` tables align on 12-char digests with wide (>40) and narrow (<12) ids.
- Byte-exact: `NO_COLOR=1` output of `list --json` equals the NDJSON frames; `BrokenPipe` (piped `logs` into `head -1`) exits `0` with no panic.
- `cargo test -p slipfork`, `cargo clippy --workspace --all-targets -- -D warnings` clean.
