---
id: M3.38
title: "Clean-sheet workspace, Constructor DI ARCHITECTURE.md, Forkfile compiler (# forkfile: 0.1), slipfork-lsp & Zed/Tree-sitter"
kind: epic
status: done
priority: 3
depends_on: [M3.37]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/**"
  - "editors/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T01:52"
---

# Goal

Scaffold the clean-sheet Rust 2024 workspace (`Cargo.toml`, `Makefile`, `ARCHITECTURE.md` with True Constructor IoC/DI and single Composition Root) and implement the complete `specs/01-forkfile.md` language stack cleanly from scratch (`<= 500` LOC per file, zero dead code):
1. **`crates/slipfork-forkfile`:**
   - Line-1 `# forkfile: 0.1` pre-`1.0` format header check.
   - Enclosing `image <id> <./dir | from <ref>> { ... }` scope (`dockerfile`, `stage`, non-secret build-time `arg` / `arg { ... }` statically rejecting `secret(...)` & secret-tainted properties, inherited `<dir>` `workdir`, `use env`, `env`, `before-start`).
   - Cargo-style `features` and boolean `on <expr>` (`!`, `&`, `|`), type-checking **all** `on` branches at once with `Forkfile:line:col` caret diagnostics and flat global symbol uniqueness across resources, `database`s, `cdn`s, `domain`s, `image`s, and `service`s.
   - Backing resources: `postgres <id> { database <db> }` (default `"18"`), `redis <id>` (default `"7"`), `s3 <bucket>` (`private` default) + optional child `cdn <id>` (`public-read`, exposing `<id>.domain` and `<id>.url`), and `domain <id>` (`<id>.domain`, `<id>.url`).
   - Strictly runtime `env` (`env <group>`, `use env`, inline `env { ... }`), `var(KEY) ?? "default"`, `secret(KEY) ?? <generator>` (`random_hex`, `random_base64`, `random_alnum`, `random_uuid`), and secret-taint tracking.
   - Cloud-identical ingress (`specs/01-forkfile.md` §6): `port <u16>` (`$port` + `PORT`) + optional `internal_ports [...]`, `route` keeping path prefixes by default (DO `preserve_path_prefix: true`), `strip-prefix`, `rewrite <path>` (mutually exclusive with `strip-prefix`), `exact <path>`, and `domain <id | prefix.*>` authority filters.
   - Uniform `: <cmd>` / `{ ... }` across `run`, `before-start [when changed(...)]`, `pre-deploy`, `post-deploy`, `failed-deploy`; `@<tool>` (`@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go`, `@cargo`) workspace-aware deduplicated `(image, lockfile_dir)` DAG node synthesis.
   - Comment-preserving `fmt` and lossless `snow.yml` -> `# forkfile: 0.1` `migrate` (`specs/05-ospp-v1.md`).
2. **`crates/slipfork-lsp` & `editors/`:**
   - `slipfork-lsp` over framed stdio (UTF-16 positions, live diagnostics, `@<tool>` & property hover tooltips, go-to-definition across `env`/`domain`/`cdn`/`database`/`s3`/`redis`, document formatting).
   - `editors/tree-sitter-forkfile` grammar + queries and `editors/zed` extension aligned 1:1 with `specs/01-forkfile.md`.
3. **Minimal `crates/slipfork` CLI bootstrap (`fmt`, `migrate`, `lsp`)** so `make check` runs cleanly from this epic onward.

# Acceptance

- `make check` exits `0` (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`).
- Focused `TESTING_PLAN.md` §7 tests cover `# forkfile: 0.1`, `image`/`arg` taint rejection, `s3 { cdn <id> }`, DO-aligned `route` rules, `@<tool>` deduplication, `fmt`, `migrate`, `slipfork-lsp`, and Tree-sitter/Zed grammar.
- Protected `crates/slipfork-forkfile/tests/critical_forkfile.rs` and `crates/slipfork-world/tests/critical_world.rs` exist, are wired, and stay green.
