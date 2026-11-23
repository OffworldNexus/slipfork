---
id: M3.85
title: "Align LSP/format tests and specs/03 with the 4-space formatter and removed adopt"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-lsp/**", "specs/03-under-the-hood.md"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:33"
updated: "2026-11-24T00:34"
---

# Problem

After `M3.84` changed the formatter to 4-space indents (`ff0a209`), `crates/slipfork-lsp/tests/critical_lsp.rs` fails on its formatted-output expectations (it passes at the pre-M3.84 base `c77a2b9`). `M3.84` did not own that crate.

Also `specs/03-under-the-hood.md` still describes up-time checkout conversion (`adopt_project`), which `M3.80` deleted: `up` never converts, and `fork` copies a plain checkout.

# Change

- Update `crates/slipfork-lsp/**` tests/fixtures that assert formatter output to 4 spaces (only assertions that encode the old width; do not reindent arbitrary input fixtures unless the test is about formatting).
- `specs/03-under-the-hood.md`: replace the adoption paragraph with the new contract — `up` calls only `ensure_store` and never modifies the checkout; `fork` snapshots a subvolume checkout and copies a plain checkout, with no adoption step.

# Acceptance

- `cargo test -p slipfork-lsp` green.
- `cargo test --workspace` no longer fails on `critical_lsp`.
- Commit `[M3.85] ...`, done, no push.
