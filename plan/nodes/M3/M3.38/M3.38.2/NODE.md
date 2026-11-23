---
id: M3.38.2
title: "slipfork-forkfile: header gate, lexer/parser/AST for all specs/01 constructs, caret diagnostics & comment-preserving fmt"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.38.1]
paths: ["crates/slipfork-forkfile/**", Cargo.toml, Cargo.lock]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T00:58"
updated: "2026-11-21T01:21"
---

# Goal
Implement the `slipfork-forkfile` front end: line-1 `# forkfile: 0.1` gate, lexer, parser and AST for every `specs/01` construct, `Forkfile:line:col` caret diagnostics, and a comment-preserving idempotent `fmt` renderer (with `--check`).

# Context
- Language surface to parse: header `specs/01-forkfile.md:75`; `image <id> <dir>` / `from <ref>` / `dockerfile` / `stage` / `arg` `:79`; `features` + boolean `on` (`!`, `&`, `|`) `:88`; resources incl. `s3 { cdn <id> }` `:92`; `env`/`use env`/`var()`/`secret()` `:106`; `port`/`internal_ports`/`route` (`strip-prefix`, `rewrite`, `exact`)/`domain` `:110`; `service`/hooks/`@<tool>` `:125`; validation + `fmt`/`lsp` scope `:131`.
- Keep every span: diagnostics must render the offending `Forkfile:line:col` with a caret underline plus one-line message and a `next:` remedy.
- AST/parser/lexer live solely in this crate (files `<= 500` LOC); no downstream re-scan of `$` tokens or route strings.
- `fmt` preserves comments, blank-line block separation and declared block order; it is idempotent and never reorders semantics.
- Semantics/typecheck belong to M3.38.3: keep parse/lex errors distinct from later semantic errors.

# Acceptance
- `cargo test -p slipfork-forkfile` green: header mismatch, unterminated string, unknown property, caret column, `on` expression precedence, and `route exact` parser cases (algorithm-only unit tests, zero mocks).
- Round-trip: `parse(fmt(parse(src))) == parse(src)` for the full sample at `specs/01-forkfile.md:6-71`.
- Library API `parse`, `fmt`, and `fmt_check` are public and idempotent; CLI wiring (`cargo run -p slipfork -- fmt --check`) lands in M3.38.4.
- `cargo clippy -p slipfork-forkfile --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `uv run --no-project tools/guard.py`.
