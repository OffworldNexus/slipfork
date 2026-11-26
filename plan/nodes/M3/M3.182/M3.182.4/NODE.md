---
id: M3.182.4
title: "Close Palette::tint to typed roles, drop boot raw prints, pty static and dead code"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.182.3]
paths:
  - "crates/slipfork/src/ui/**"
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/src/commands/pty.rs
  - crates/slipfork/src/commands/features.rs
  - crates/slipfork/src/commands/fmtmigrate.rs
  - "crates/slipfork/src/doctor/**"
  - "crates/slipfork/src/status/**"
  - tools/guard.py
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T21:45"
updated: "2026-11-25T22:23"
---

# Goal

Close the raw-SGR doorway, remove the `boot.rs` print allowlist entry honestly, delete the one `static` mutable, and delete dead `#[allow(dead_code)]` residue.

# Context (pre-baked, verified at HEAD 4173bd7)

- `crates/slipfork/src/ui/report/palette.rs:47` `pub fn tint(text, code)` -> private `fn tint` plus named public role methods covering every external call: `boot/render.rs:119,288,289,326,339,352,353,365,366`, `boot/phase.rs:127`, `boot/build/tests/roles.rs:323,358`, `commands/features.rs:171,181`, `commands/fmtmigrate.rs:242`, `doctor/mod.rs:81,84`, `status/tree.rs:306`, and `ui/report/{failure.rs:173,224, render.rs:214,215,228, row.rs:147,153, stream.rs:332}`. Report-internal modules may keep the private `tint`.
- Needed roles already exist (`label`, `value`, `url`, `meta`, `path`, `ok`, `warn`, …) plus new ones for green check marks, red failures, bold-green readiness, and bold-cyan commands; `commands/fmtmigrate.rs:242` styles arbitrary diff lines and needs a typed `diff_line(added|removed|context)`-style role, never a raw code.
- `crates/slipfork/src/boot.rs:159,202,221,360` raw prints: route through the boot append sink (the reason already recorded in `tools/guard.py`), then remove the `crates/slipfork/src/boot.rs` entry from `RAW_PRINT_ALLOWLIST` (`tools/guard.py:52-70`). Never widen the allowlist.
- `crates/slipfork/src/commands/pty.rs:18` `static RESIZED: AtomicBool`: signal handler at 21, single reader at 92 -> own the flag as a field on the pty struct (installed by its owner) instead of a `static`.
- Delete dead `#[allow(dead_code)]` residue: `crates/slipfork/src/boot/render.rs:241 status_line` (zero non-test callers) and any sibling dead item this slice exposes.

# Acceptance

- `grep -rn 'tint(' crates/*/src` shows no string-literal SGR argument outside `ui/report/palette.rs`.
- `uv run --no-project tools/guard.py` exits 0 and `crates/slipfork/src/boot.rs` is absent from `RAW_PRINT_ALLOWLIST`.
- `grep -n "static " crates/slipfork/src/commands/pty.rs` is empty.
- `NO_COLOR=1` and `--json` captures emit zero `ESC[` bytes for `status`/`doctor`; PTY goldens for `up`/`fork`/`down`/`status` unchanged; `cargo test -p slipfork` green; `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Do **not** touch `crates/slipfork/src/daemon/**`, `crates/slipfork/src/world/**`, `specs/`, `TESTING_PLAN.md`, `CHANGELOG.md`, `docs/`, or any `critical_*.rs`.
