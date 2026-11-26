---
id: M3.146
title: "Boot/daemon facts: StepSettlement, FailureFacts, ui/report/live.rs over BootProgress"
kind: task
status: superseded
owner: implementer
priority: 2
depends_on: [M3.141, M3.145]
paths:
  - "crates/slipfork/src/ui/report/**"
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/supervise.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T03:42"
updated: "2026-11-25T17:06"
---

# Goal
Slice 2 of `specs/02-cli-ux.md` §9.5: make the daemon→CLI boundary typed. `daemon/reconcile/**` emits `StepSettlement`/`FailureFacts`; `boot.rs` and every `boot/**` renderer emits `ui/report/live.rs` rows over `BootProgress` + `Pane`; `world/{up,fork,mod,supervise}.rs` stop formatting. Add `ConnectNotice::EphemeralDaemon`.

# Context
- Depends on `M3.141` (owns `daemon/reconcile/**`, `daemon/client.rs`, `boot/**`, `world/**`) and `M3.145` (report core).
- Replaces/deletes: `boot/failure.rs` (whole file), `Node { success, state }`, `ImageEnd`, the `ImageTally::self.total` bug, `world::fail`, and `boot.rs`'s own `Palette`/`println!` path.
- `Palette`/`println!` anchors inside `crates/slipfork/src`: `boot.rs:185,224,235,260,274,285,294,301,308,400,441,472`; `boot/pane.rs:30,156`; `boot/render.rs:146,264,282,353,365,379,391,414`; unit-test sites `boot/failure.rs:86,103,116,132,147,169` and `boot/build/tests/{roles,layout}.rs` — rewrite these unit tests on `Report` values (keep them pure; no new lab boots).
- `boot.rs` is 490 LOC: split the DECSTBM pane repaint into `ui/report/live.rs` so both files stay <500 LOC and the repaint is `O(active + 8)` (§9.4).
- `supervise.rs` keeps plain log stderr (detached journal exemption); it must construct zero `Palette`.
- Do **not** touch `crates/slipfork/tests/critical_*.rs` — the zero-edit invariant is part of this slice's proof.

# Acceptance
- `grep -rn 'Palette::\|println!\|eprintln!' crates/slipfork/src/boot.rs crates/slipfork/src/boot` → only the documented detached-journal exemption (zero in `boot/render.rs`, `boot/pane.rs`, `boot.rs`).
- Boot unit tests assert on `Report`/`StepSettlement`/`FailureFacts` (missing `ImageEnd`/`Node.state` types fail to compile if reintroduced).
- PTY proof: one `up` journey with `TZ=Europe/Paris` asserts (a) `[i/n]` outer and `n/N` inner lowercase alignment with no sheared ANSI, (b) repaint line count <= active phases + 8 + 1, (c) `--json` NDJSON `StepSettlement` frames.
- `git diff --stat -- crates/slipfork/tests` shows no `critical_*.rs` change.
- `cargo test -p slipfork` and `cargo clippy --workspace --all-targets -- -D warnings` clean.
