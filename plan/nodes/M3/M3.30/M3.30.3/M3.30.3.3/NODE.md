---
id: M3.30.3.3
title: "Dynamic ANSI-stripped column widths across status/ps/list/forward/secret + plugin ANSI purity"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.3.2]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**", "packages/opencode-slipfork/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T01:25"
updated: "2026-11-19T02:02"
---

# Goal
Section 3 of `plan/nodes/M3/M3.30/M3.30.3/NODE.md`: every table fits its real content.

# Work
- Replace hardcoded widths ({:<28}, {:<40}, {:<24}, LABEL_WIDTH) with ANSI-stripped visible-width .max() computation in commands/{inspect,status,ps,forward,secret}.rs and doctor.rs.
- Keep one-line + next: errors; gate [resources] jargon behind -v.
- packages/opencode-slipfork: keep plugin output ANSI-free when not a TTY; delete the stale BROWSER=/bin/true coupling in tests/e2e.critical.test.ts.

# Constraints
No color change beyond TerminalUi; --json untouched. No RESULT.md.

# Acceptance
- no hardcoded width literals in commands/ or doctor.rs
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test -p slipfork --lib
- bun test packages/opencode-slipfork/tests
