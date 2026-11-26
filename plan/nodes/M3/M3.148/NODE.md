---
id: M3.148
title: "Status/doctor/commands on Report + narrow ui helpers + guard.py no-raw-print gate"
kind: task
status: superseded
owner: implementer
priority: 4
depends_on: [M3.141, M3.147]
paths:
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/ui/palette.rs
  - crates/slipfork/src/ui/paint.rs
  - crates/slipfork/src/ui/table.rs
  - crates/slipfork/src/ui/failure.rs
  - "crates/slipfork/src/status/**"
  - "crates/slipfork/src/doctor/**"
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/commands/auth.rs
  - crates/slipfork/src/commands/env.rs
  - crates/slipfork/src/commands/secret.rs
  - crates/slipfork/src/commands/features.rs
  - crates/slipfork/src/commands/open.rs
  - crates/slipfork/src/commands/fmtmigrate.rs
  - tools/guard.py
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/doctor_surface.rs
  - crates/slipfork/tests/tree_domain_surface.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T03:42"
updated: "2026-11-25T18:26"
---

# Goal
Slice 4 of `specs/02-cli-ux.md` §9.5: finish the migration (`status`, `doctor`, `env`, `secret`, `features`, `open`, `fmt`/`migrate`, `commands/mod.rs`) and close the door: narrow `Palette`/`paint`/`table` to `pub(super)` inside `ui`, delete the last legacy types, and enable the mechanical no-raw-print gate in `tools/guard.py`.

# Context
- Depends on `M3.141` (`commands/**`) and `M3.147`; it touches `ui/{palette,paint,table,failure}.rs`, which slice 1 also touched, hence strict serialization.
- Migrate anchors (`crates/slipfork/src`): `status/tree.rs:134,139`, `status/model.rs:8` (`ui::paint`); `doctor/mod.rs:76,186`; `commands/mod.rs:231,232,313,315`; `commands/secret.rs:91,183,215,245`; `commands/env.rs:92`; `commands/features.rs:148`; `commands/open.rs:38,63,64,70,76,77`; `commands/fmtmigrate.rs:98,103,109,114,141,180,210,219,225`.
- Deletions: `commands/auth.rs` (dormant), `ui/failure.rs` `split_remedy`/`failure_block` (after slice 1 + 2 stop calling it), `LocalClock`/`LocalClock::Fixed`/`Session::clock`, `status::rows::State`, `Session::{fail,report,report_details}`.
- `tools/guard.py`: add one `RawPrint` rule over `crates/slipfork/src/**` rejecting `\b(print|println|eprint|eprintln|write)!` outside `crates/slipfork/src/ui/report/**`, plus `Palette::` and `ui::paint` outside `ui/`; allowlist exactly three things — `clap` `--help`/`--version` rendering, `ui::prompt_secret` (no echo), and detached `__daemon`/`__supervise`/`__ingress` journal stderr. Slice 4 enables it; keep the rule table small (no per-file ad-hoc exemptions).
- `doctor` leads with `✓ Ready` / `✗ N issue(s)`; port 80 URLs omit `:80`; `[resources]` detail gated behind `-v`.
- Do **not** touch `crates/slipfork/tests/critical_*.rs`.

# Acceptance
- `grep -rn 'Palette::\|ui::paint' crates/slipfork/src --include=*.rs | grep -v 'src/ui'` → no matches; `ls crates/slipfork/src/commands/auth.rs` → gone; `grep -rn 'LocalClock\|status::rows::State' crates/slipfork/src` → no matches.
- `uv run --no-project tools/guard.py` exits 0 and its `RawPrint` rule reports 0 violations; temporarily reintroduce one `println!` outside `ui/report/` locally to confirm the gate fires (do not commit it).
- PTY/byte-exact proof: `status`/`ps` trees (one blank line between worlds, `○ No worlds running`), `doctor` verdict line, `secret`/`env`/`features` tables and `logs -r` headers compared byte-exactly with `TZ=Europe/Paris` and `NO_COLOR=1`; `--json` projections unchanged.
- `cargo test -p slipfork`, `cargo clippy --workspace --all-targets -- -D warnings`, `uv run --no-project tools/guard.py` clean.
