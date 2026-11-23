---
id: M3.30.3.1
title: "TerminalUi palette + clap Styles + ANSI purity + verdict-first doctor"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T01:25"
updated: "2026-11-19T01:43"
---

# Goal
Section 1 of `plan/nodes/M3/M3.30/M3.30.3/NODE.md` (read it): one `TerminalUi` struct owns capability detection and styling.

# Work
- Add `owo-colors` + `indicatif` to the workspace.
- Create `crates/slipfork/src/ui.rs` `TerminalUi`: IsTerminal detection, NO_COLOR, --json, -v, spinner handle, styling helpers. Zero ANSI bytes when piped / NO_COLOR=1 / --json; bold cyan URLs (omit :80), green running/check, yellow bang, red failed/cross, dim 12-char digests on TTY only.
- Clap Styles where clap lives, driven by the same capability detection.
- doctor: lead with a verdict line, then checks, then next remedies; advisory checks subordinated to fatal ones.
- Wire `ui` through `CliApp` (already has a `ui` field).

# Constraints
Concrete struct, no trait; no ambient reads outside the root; pure palette unit tests. No RESULT.md.

# Acceptance
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test -p slipfork --lib ui::
- NO_COLOR=1 status output contains zero escape bytes; --json status is valid JSON
