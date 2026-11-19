---
id: M3.32.11
title: "editors/tree-sitter-forkfile grammar + editors/zed extension + tools/check_zed_extension.py"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.10]
paths:
  - "editors/**"
  - tools/check_zed_extension.py
  - "tools/tests/**"
  - tools/do_vm.py
  - support/demo/README.md
  - Makefile
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T16:03"
updated: "2026-11-19T22:20"
---

# Goal

Real editor support: committed tree-sitter grammar and a Zed extension that launches `slipfork-lsp`.

# Context

OWNER ADDITION to `M3.32` (blueprint from the `Blastfile` worktree). Zed reads committed `src/parser.c`, never `grammar.js`.

- `editors/tree-sitter-forkfile/`: `grammar.js` (`name: "forkfile"`, `extras: [/[ \t\r]/, $.comment]` — NOT `\s`, newlines terminate statements), `package.json`, corpus tests, and committed generated `src/{parser.c,grammar.json,node-types.json}` + `src/tree_sitter/parser.h` (`bunx tree-sitter-cli generate && test`, or `npx`).
- `editors/zed/`: `Cargo.toml` (`crate-type=["cdylib"]`, `zed_extension_api="0.7.0"`, EMPTY `[workspace]`) + committed `Cargo.lock` + `LICENSE` symlink to `../../LICENSE`; `extension.toml` (`id="forkfile"`, `name="Forkfile"`, `schema_version=1`, `[grammars.forkfile] path="editors/tree-sitter-forkfile"`, `[language_servers.slipfork-lsp] languages=["Forkfile"]`, `language_ids.Forkfile="forkfile"`); `src/lib.rs` `language_server_command` resolving lsp config path -> `worktree.which("slipfork-lsp")` -> `worktree.which("slipfork")` with `args:["lsp"]` -> GitHub release fallback for `OffworldNexus/slipfork`, env allowlist `["PATH","SYSTEMROOT"]`.
- `languages/forkfile/{config.toml,highlights.scm,brackets.scm,indents.scm,outline.scm}` (`path_suffixes=["Forkfile","forkfile"]`, `#` comments, ~14 theme captures with `@variable.special @variable` fallback; keep `highlights.scm` in sync with the tree-sitter queries) + `.zed/settings.json` (`format_on_save: on`).
- `tools/check_zed_extension.py` (stdlib) + pytest wired into `make check`: manifest validity, query-capture sync, `path_suffixes`, empty `[workspace]`, and `cargo check --manifest-path editors/zed/Cargo.toml`.

# Acceptance

- `uvx tree-sitter test`/`bunx` grammar corpus passes; `cargo check --manifest-path editors/zed/Cargo.toml` exits 0; `uv run --no-project tools/check_zed_extension.py` exits 0; `make check` green.
- Write NO `RESULT.md`.
