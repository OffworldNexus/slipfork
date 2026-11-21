---
id: M3.38.5
title: "slipfork-lsp framed-stdio server, Tree-sitter grammar/queries & Zed extension 1:1 with specs/01"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.38.4]
paths:
  - "crates/slipfork-lsp/**"
  - "editors/**"
  - "crates/slipfork/**"
  - "crates/slipfork-forkfile/**"
  - Cargo.toml
  - Cargo.lock
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T00:58"
updated: "2026-11-21T01:52"
---

# Goal
Ship `slipfork-lsp` over framed stdio plus a first-party Tree-sitter grammar and Zed extension so `# forkfile: 0.1` is a first-class editor language.

# Context
- Server scope: `specs/01-forkfile.md:131` (`slipfork fmt`, `slipfork lsp`, Zed extension); focused coverage contract: `specs/04-testing-plan.md:70-75` (live diagnostics, `@<tool>` hover, go-to-definition, format-on-save).
- The server reuses `slipfork-forkfile` parse/typecheck/`fmt` (no duplicated grammar logic in Rust) and needs no port/adapter: `specs/03-under-the-hood.md:116-124`.
- Grammar must mirror `specs/01` exactly: header `:75`, `image`/`arg` `:79`, `features`/`on` `:88`, resources `:92`, `env`/`secret` `:106`, `port`/`route`/`domain` `:110`, `service`/hooks/`@<tool>` `:125`.
- Framed stdio: `Content-Length` headers, UTF-16 code-unit positions, incremental `didChange`, `publishDiagnostics` with caret ranges, `textDocument/formatting`, `textDocument/definition`, `textDocument/hover` for `@<tool>` and routed properties.
- Deliver `editors/tree-sitter-forkfile/{grammar.js,src/,queries/highlights.scm,queries/locals.scm,test/corpus/}` and `editors/zed/{extension.toml,languages/forkfile/...}` pointing at the shipped `slipfork lsp` binary.
- Protected test path `crates/slipfork-lsp/tests/critical_lsp.rs` is guarded by `tools/critical_guard.py`.

# Acceptance
- `cargo test -p slipfork-lsp` green, including a real stdio child-process handshake: `initialize` -> `didOpen` with a bad `route` -> diagnostics with a caret range -> `textDocument/formatting` edit -> shutdown/exit.
- UTF-16 position tests cover astral emoji and accented identifiers in `Forkfile` strings/comments.
- `cargo test -p slipfork-lsp --test critical_lsp` green (live diagnostics + hover + definition + formatting).
- Grammar corpus: `tree-sitter test` in `editors/tree-sitter-forkfile` passes (`uvx --offline tree-sitter test` acceptable); queries mention every `specs/01` keyword; `editors/zed` config is 1:1 with the grammar file types.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `uv run --no-project tools/guard.py`.
