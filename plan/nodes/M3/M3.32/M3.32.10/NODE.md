---
id: M3.32.10
title: "slipfork_forkfile::fmt + crates/slipfork-lsp (zero-tokio framed stdio, UTF-16) + slipfork lsp"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.8]
paths:
  - "crates/slipfork-lsp/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork/**"
  - "docs/design/dx-cli-0.1.0.md"
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T16:03"
updated: "2026-11-19T16:03"
---

# Goal

Ship a real `Forkfile` language server: canonical formatter, framed JSON-RPC over stdio, and `slipfork lsp`.

# Context

OWNER ADDITION to `M3.32` (ported from the `Blastfile` worktree at `~/offworld-nexus/blastit/.worktrees/release-0.2.0`; readable if a detail is unclear).

- `slipfork_forkfile::fmt(source: &str) -> Result<String, Vec<Diagnostic>>`, idempotent, reuses the canonical parser/checker.
- New `crates/slipfork-lsp` (`#![forbid(unsafe_code)]`, modules <= 500 LOC: `framing.rs`, `utf16.rs`, `hover.rs`, `server.rs`, `lib.rs`, `main.rs`; deps ONLY `slipfork-forkfile` + `serde_json`; ZERO `tokio`). Exports the standalone `[[bin]] slipfork-lsp` AND `slipfork_lsp::run()`, wired to `slipfork lsp`.
- Framed `Content-Length: N\r\n\r\n` UTF-8 byte loop: 8 KiB header cap, 2 MiB body cap, `catch_unwind`, never panics, never echoes document/secret text to stderr, exits 0 after `shutdown`+`exit`.
- CRITICAL: Zed ignores `positionEncoding` and always speaks UTF-16. Convert 1-based `char` columns through `char::len_utf16`; include a non-BMP emoji test.
- Capabilities: `textDocumentSync` openClose + full sync, push `publishDiagnostics` (all multi-error diagnostics), `textDocument/formatting` via `fmt`, `textDocument/hover` for keywords/resources/props/`secret(...)`/`var(...)`, plus completion/definition if clean.
- Protected `critical_lsp.rs`: spawn the real `slipfork lsp` binary over pipes; valid/invalid diagnostics, emoji UTF-16 column, hover, formatting, malformed JSON, clean shutdown/exit.

# Acceptance

- `cargo test -p slipfork-lsp -p slipfork-forkfile -p slipfork` exits 0; `cargo test --workspace --all-targets --no-run` links; new binary registered in the Makefile suite list where required.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
