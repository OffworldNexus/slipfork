---
id: M3.32.4.1
title: "Unified bare slipfork tree, ps, ui widths, -F features and short flags"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-wire/**"
  - "crates/slipfork-forkfile/src/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
attempts: 5
max_attempts: 6
escalation: none
created: "2026-11-19T21:01"
updated: "2026-11-19T21:19"
---

# Goal
Bare `slipfork` renders one World -> Services -> Forks tree with zero PID/lineage/next spam and `○ No worlds running` at exit 0; `ps` scopes to cwd; dynamic ANSI-stripped widths; delete `list`; add `-F/--features`, `--no-default-features` and the short flags. Spec: `docs/design/dx-cli-0.1.0.md` §1-§2.

# Context
Command enum `crates/slipfork/src/cli.rs:32,100,180 (List),297 (Resolve)`; presenters `src/status/{mod,table,lineage_table}.rs`; `src/ps.rs`; `src/ui.rs`; `src/main.rs:35` injects color. Lineage hashes only behind `-v`; zero `slipforkd <pid>` header.

# Acceptance
- `cargo test -p slipfork --all-targets` exits 0; clippy `-D warnings`; fmt clean.
- `! grep -rn "List," crates/slipfork/src` exits 0.
- UI tests assert zero ANSI escape bytes under `NO_COLOR`, piped, and `--json`; `--json` shapes covered for bare/`status` and `ps`.
- Write NO `RESULT.md`.
