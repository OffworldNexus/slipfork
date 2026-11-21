---
id: M3.40.1
title: "Standalone multi-platform slipfork-lsp binary; delete the slipfork lsp verb; Zed PATH/download"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/main.rs
  - crates/slipfork/Cargo.toml
  - "crates/slipfork/tests/**"
  - Cargo.toml
  - Cargo.lock
  - "editors/zed/**"
  - Makefile
  - ARCHITECTURE.md
  - "specs/01-forkfile.md"
  - "specs/02-cli-ux.md"
  - "specs/06-distribution-and-docs.md"
  - TESTING_PLAN.md
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T12:56"
---

# Goal
Ship `slipfork-lsp` as a standalone, portable binary for Linux/macOS/Windows, delete the `slipfork lsp` verb, and make the Zed extension find or download that binary.

# Context
- `crates/slipfork-lsp` is lib-only: no `[[bin]]` in `crates/slipfork-lsp/Cargo.toml`; `Server<W,R>::new` + `serve()` live at `crates/slipfork-lsp/src/server.rs:33-50`.
- Verb plumbing to delete: `crates/slipfork/src/cli/parse.rs:46` (`"lsp" =>`), `parse.rs:219` `lsp_verb`, `crates/slipfork/src/cli/mod.rs:31` (`Lsp`) and the `HelpTopic::Lsp` variant, `crates/slipfork/src/cli/help.rs:23` + `:131-146`, `crates/slipfork/src/commands/mod.rs:88` `pub fn lsp`, its dispatch arm, and `crates/slipfork/tests/critical_cli_surface.rs` expectations.
- `editors/zed/extension.toml` declares `[language_servers.slipfork] name = "slipfork lsp"` with no download logic and no `editors/zed/src/`.
- Mirror `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/editors/zed/src/lib.rs` (733 lines): PATH lookup first, else GitHub Release asset download per platform (linux/darwin/windows x x86_64/aarch64), `ENV_ALLOWLIST = PATH + SYSTEMROOT`.
- `crates/slipfork-lsp/src/main.rs` becomes the only composition root for LSP (argv/stdin/stdout only); library modules keep zero `std::env`.
- `packaging/` and `scripts/` are outside this epic's paths: the binary name `slipfork-lsp` must be reachable from the existing Makefile/standalone build targets, nothing more.

# Acceptance
- `cargo build -p slipfork-lsp --bin slipfork-lsp` and `cargo test -p slipfork-lsp`.
- `cargo test -p slipfork --test critical_cli_surface`; `slipfork lsp` now exits 2 with `unknown argument 'lsp'` and no `lsp` row in `slipfork --help`.
- Portability guard: `cargo tree -p slipfork-lsp -e normal | grep -E '^(nix|crun)'` is empty (no Linux-only deps reachable from `slipfork-forkfile` + `slipfork-lsp`).
- `cargo clippy -p slipfork-lsp -p slipfork --all-targets -- -D warnings`.
- Zed: `grep -n 'slipfork-lsp' editors/zed/extension.toml editors/zed/src/lib.rs` hits both; `! grep -rn 'slipfork lsp' editors/zed specs ARCHITECTURE.md TESTING_PLAN.md`.
- `uv run --no-project tools/critical_guard.py`.
