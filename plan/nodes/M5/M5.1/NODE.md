---
id: M5.1
title: "Shared editor/agent control substrate (daemon.sock typed RPC + slipfork-mcp) & thin IDE adapters"
kind: epic
status: ready
priority: 3
depends_on: [M4]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T17:47"
updated: "2026-11-25T19:21"
---

# Goal

Design (`specs/` first) and build the shared editor/agent control substrate — versioned typed RPC on `<runtime_dir>/daemon.sock` plus one `slipfork-mcp` server — then decompose thin adapters for **Zed**, **VS Code**, and **JetBrains** after `M4` (`0.2.0` `slipfork test`) ships.

# Context & Precedent

- **Gated on `M4` (`0.2.0`):** Do not activate or decompose per-editor slices until `M4` (`slipfork test` + `slipfork auth`) is `done`.
- **Thin-adapter precedent:** `packages/opencode-slipfork/` (`@slipfork/opencode`) delegates all world/fork state to `slipfork` without owning ledger or secret logic.

# Acceptance

- [ ] `specs/` contract accepted before per-editor child nodes are created.
- [ ] `uv run --no-project tools/plan.py validate`, `make check`, `make live-test`, and `make docs-check` exit 0 with real lab E2E coverage over the daemon RPC and `slipfork-mcp`.
