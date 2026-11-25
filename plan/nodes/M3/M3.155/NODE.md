---
id: M3.155
title: "fmt must keep one blank line between items (collapse runs, never invent extras)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/src/fmt/**"
  - "crates/slipfork-forkfile/src/tests/**"
  - "crates/slipfork-forkfile/src/parser.rs"
  - "crates/slipfork-forkfile/src/parser/**"
  - "crates/slipfork-forkfile/src/service.rs"
  - "crates/slipfork-forkfile/src/ast.rs"
  - "crates/slipfork-forkfile/src/resources.rs"
  - "crates/slipfork-forkfile/src/migrate/render.rs"
  - "crates/slipfork-forkfile/tests/critical_forkfile.rs"
  - docs/forkfile.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T10:15"
updated: "2026-11-25T10:37"
---

# Owner report

"I get the feeling that fmt is eating up blank lines? I like blank lines... at least 1 should be allowed."

# Cause

`crates/slipfork-forkfile/src/fmt/` renders from the AST, and the AST records no blank lines, so `slipfork fmt` drops every user blank line except the structural ones it inserts itself (`render.rs::blank`, `fmt/mod.rs`).

# Fix (no new AST node needed)

Every item carries a `Span` with 1-based line numbers (`span.rs`). The formatter can recover intent:

- Between two consecutive items in the same body, if the source had **one or more** blank lines (`next.span.start.line > previous.span.end.line + 1`), emit exactly **one** blank line. Collapse runs of 2+ to 1.
- Do not invent blanks where the source had none, and do not add blanks across bodies/blocks beyond today's structural rules.
- Remove trailing blank lines at the end of a block/body and end the file with exactly one newline.
- Idempotence: `fmt(fmt(x)) == fmt(x)`, including for a file that already has single blanks; `fmt --check` reports no diff after one pass.
- Comments attached to items keep their adjacency rules (a comment directly above its item stays attached; a blank between a comment and an item is preserved as authored).

# Acceptance

- Byte-exact tests: a file with blanks between services/args keeps exactly one; two+ blank lines collapse to one; no blanks in the source produce no new blanks; comments stay attached; trailing blanks removed; formatting twice is a no-op.
- `slipfork fmt --check` on the repo's `examples/*/Forkfile` takes one pass and changes nothing after the first run.
- `make check`; Triad (ux: no gratuitous whitespace churn); commit `[M3.155] ...`, done, no push.
