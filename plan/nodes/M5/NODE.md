---
id: M5
title: "Editor & IDE Integration (VS Code, JetBrains, Zed) (0.5.0)"
kind: milestone
status: ready
priority: 5
depends_on: [M4]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T17:47"
updated: "2026-11-17T17:47"
---

# Goal

Deliver Slipfork `0.5.0`: deep, zero-cruft editor and IDE integrations for **VS Code**, **JetBrains (IntelliJ Platform)**, and **Zed** (plus every MCP-capable AI coding agent) over **one shared control plane** (`slipforkd`'s `DaemonControl` typed editor RPC + `slipfork-mcp`), scheduled strictly **after** `0.2.0` (`STP v1`, `M3.2`) and `M4` (`0.4.0`).

# Context & Architectural Invariants

- **Dependency honesty:** IDE plugins are only worth building once `0.2.0`'s `STP v1` (`M3.2`) exists — per-world test runs, protocol-native coverage, and `mutants` alongside live worlds and logs are what an IDE should show. `M5` depends explicitly on `M4` (which follows `M3` / `0.2.0`).
- **One control plane, N thin adapters:** Following `packages/opencode-slipfork/` (`@slipfork/opencode`, `ADR-0004`), editors consume a *versioned, typed* editor RPC over `slipforkd`'s existing `DaemonControl` (`ARCHITECTURE.md` §1, `ADR-0008` §4) — zero per-editor business logic, and zero shelling out or scraping CLI text for state. One `slipfork-mcp` server (`list_worlds`, `up`, `fork`, `restart`, `logs`, `db_query`, `open_url`, `exec`) makes Zed and every AI agent (Zed agent panel, OpenCode, Claude Code, Codex) first-class *for free*, while VS Code and JetBrains put thin native UI over the same protocol.
- **Deferred decomposition:** Placeholder epic `M5.1` captures the shared substrate and per-editor UX intent; per-editor epics and leaf tasks wait for a dedicated `dx-first` (`docs/design/dx-editor-ide.md`) + `architect` + `planner` pass after `0.2.0` ships.
- **Zero vaporware:** Zero `docs/` pages and zero `README.md` claims about editor/IDE plugins until the first integration compiles, passes critical suites, and ships; `docs/VISION.md` is the sole home until then.

# Acceptance

- [ ] `make check` and `make docs-check` exit 0, and every `M5` (`0.5.0`) exit criterion in `plan/ROADMAP.md` is verified.
