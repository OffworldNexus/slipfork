---
id: M3.141
title: "ps lineage renderer, broken-row remedies, offline reader, specs/02 sync, live VM ghost-state proof"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.140]
paths:
  - crates/slipfork/src/ps.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - "crates/slipfork/src/status/**"
  - crates/slipfork/src/world/target.rs
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/cli/verbs.rs
  - packaging/man/slipfork.1
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/tree_domain_surface.rs
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/ansi_matrix.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T01:49"
updated: "2026-11-25T11:50"
---

# Goal

`ps`/`status` render ledger lineage (`salut ← bonjour ← test1`), broken entries with exactly one remedy, and no phantom rows; ship `specs/02-cli-ux.md` §3/§7 samples in sync; prove every ghost state from the owner's transcript on a live VM. Renderer + offline reader only — no daemon writes.

# Context

- `crates/slipfork/src/status/tree.rs:128 render`, `:162 scope`; `status/rows.rs:47 State`, `:72 project`; `status/model.rs:41 with_verbose` currently gates lineage behind `-v`, but `specs/02-cli-ux.md:64-97` prints dim `← parent ← root` unconditionally and `lineage: [...]` in `--json` — remove the gate.
- `crates/slipfork/src/world/target.rs:43-121` `Targets` (bare-name resolution inside/outside a family, ambiguous `next: slipfork drop <name>--<root>`) moves to ledger rows from `PsReply`.
- Offline path: `status`/`ps` with no daemon must call `StateDb::open_reader` and never mutate or converge (`crates/slipfork/src/commands/mod.rs` dispatch); empty DB prints `○ No worlds running`, exit `0`.
- Broken rows: `! checkout missing — run 'slipfork drop <fork>'` (fork) / `restore it or run 'slipfork down <world>'` (root) / `! Forkfile invalid: <line>:<col>: <message>`; a `!` row has zero sub-rows and no fabricated `orphaned`.
- Docs: `specs/02-cli-ux.md:64-97` (§3 ps/status samples), `:116-120` (plan frame), `:247-250` (`--json` payloads) — `slipfork ps` sample must show the 3-world lineage block and the post-`drop bonjour` reparented view.
- Styling rules: compute widths with ANSI-aware `.max()`, `←` and stopped rows dim, `✗`/`!` red, zero `\x1b[` under `NO_COLOR`/non-TTY/`--json`.
- Tests: `crates/slipfork/tests/critical_status_live.rs`, `status_surface.rs`, `tree_domain_surface.rs`, `critical_fork_live.rs`, `ansi_matrix.rs`.

# Acceptance

- `cargo test -p slipfork`; `cargo clippy --workspace --all-targets -- -D warnings`; `uv run --no-project tools/guard.py`; `uv run --no-project tools/check_docs_honesty.py` if docs changed.
- VM proof (full §7.5 journey, `tools/do_vm.py ssh`, private tree, never `do_vm.py sync`, never `~/demo`): boot `test1` → `fork bonjour` → `cd .forks/bonjour && fork salut` → `cd ../salut && fork merci`; assert flat `.forks`, `ps` shows `merci ← salut ← bonjour ← test1`; `drop bonjour -f` then `ps` shows `salut ← test1` and `merci ← salut ← test1` still `200 OK`; delete `test1/.forks/merci` while running → `ps` shows `! checkout missing` with pods reaped and one remedy; plant a `creating` row + untracked `worlds/ghost--test1`, restart daemon → clean `ps`.
