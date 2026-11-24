---
id: M3.84
title: Formatter default indent is 4 spaces
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - crates/slipfork/tests/critical_fmt_migrate.rs
  - support/demo/Forkfile
  - "editors/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:19"
updated: "2026-11-24T00:22"
---

# Goal

`slipfork fmt` renders 2-space indents. The owner wants the **default formatter indent to be 4 spaces**.

# Change

- `crates/slipfork-forkfile/src/fmt/render.rs`: `Renderer::line` writes `"  "` per level — make it `"    "` (4 spaces); update the `fmt/mod.rs:33` "2-space indents" doc comment.
- Reformat `support/demo/Forkfile` with the new formatter (and any other checked-in `Forkfile`).
- Update every formatter golden that asserts output: `crates/slipfork-forkfile/tests/**` (fmt/round-trip goldens) and `crates/slipfork/tests/critical_fmt_migrate.rs` (fmt, `--check`, `migrate` output). Parsing accept tests and 2-space input fixtures stay valid — do not mass-reindent every fixture.
- Editor defaults: if `editors/vim` or `editors/zed` pin an indent width (e.g. `shiftwidth=2`), move it to 4. Tree-sitter `indents.scm` only marks nodes, leave it.

# Out of scope (blocked by M3.79, do later)

`docs/forkfile.md`, `specs/01-forkfile.md`, `README.md` examples still show 2-space blocks. Publish the new canonical indent in a docs pass once M3.79 lands.

# Acceptance

- `cargo test -p slipfork-forkfile` and `cargo test -p slipfork --test critical_fmt_migrate` green.
- `slipfork fmt` on a sample emits 4-space nested blocks; `fmt --check` on the reformatted demo is clean; `make check` green.
- Commit `[M3.84] ...`, done, no push.
