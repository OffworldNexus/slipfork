---
id: M3.40.6
title: "packages/opencode-slipfork (@slipfork/opencode) V2 plugin + lsp/opencode docs sweep"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.5]
paths:
  - "packages/opencode-slipfork/**"
  - "specs/02-cli-ux.md"
  - "specs/06-distribution-and-docs.md"
  - ARCHITECTURE.md
  - TESTING_PLAN.md
  - Makefile
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T12:50"
---

# Goal
Ship `packages/opencode-slipfork` as `@slipfork/opencode`, an OpenCode V2 plugin that forks and reaps Slipfork worlds for parallel agents, with docs aligned.

# Context
- `packages/` does not exist yet; create the workspace (`package.json` name `@slipfork/opencode`, TS config, test runner consistent with the Makefile conventions).
- Hooks to implement: `ctx.worktree.transform` (create `slipfork fork <name>`), `ctx.shell.hook`, `ctx.session.hook` (reap with `slipfork drop <name>`).
- The plugin shells out to the shipped `slipfork` binary resolved from PATH (allowlist PATH + SYSTEMROOT only, mirroring `editors/zed/src/lib.rs`); no second CLI, no embedded world logic.
- Integration tests must drive the real `slipfork` binary built from this workspace with a temp `XDG_RUNTIME_DIR` under `/var/tmp` - zero mocks, fakes, recordings, or stubbed binaries.
- Docs sweep: `specs/02-cli-ux.md`, `specs/06-distribution-and-docs.md`, `ARCHITECTURE.md`, `TESTING_PLAN.md` must describe only shipped behavior (standalone `slipfork-lsp`, no `slipfork lsp` verb, real plugin hooks).

# Acceptance
- `cargo build -p slipfork` then the plugin suite (`bun test` or the repo-standard runner wired as `make packages-check`) passes against the real binary: fork on session hook, drop on session end, no leaked worlds.
- `make packages-check` exits 0; `uv run --no-project tools/check_docs_honesty.py` passes.
- `! grep -rn 'mock\|fake\|stub' packages/opencode-slipfork/src`.
- `uv run --no-project tools/critical_guard.py`.
