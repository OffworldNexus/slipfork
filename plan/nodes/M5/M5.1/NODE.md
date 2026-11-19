---
id: M5.1
title: "Shared editor control substrate (DaemonControl typed editor RPC + slipfork-mcp) & per-editor DX intent"
kind: epic
status: ready
priority: 3
depends_on: [M3.2, M4]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T17:47"
updated: "2026-11-17T17:47"
---

# Goal

Design and build the shared editor control substrate — a versioned, typed editor RPC over `slipforkd`'s `DaemonControl` plus one `slipfork-mcp` server — and run the post-`0.2.0` `dx-first` + `architect` + `planner` pass to decompose deep thin adapters for **VS Code**, **JetBrains**, and **Zed**.

# Context & Precedent

- **Gated on `0.2.0` (`STP v1`, `M3.2`) and `M4`:** IDE plugins are only worth building once `0.2.0`'s `STP v1` exists (per-world test runs, protocol-native coverage, and `mutants` are the high-signal state an IDE should show alongside running environments and logs). Do **not** activate, escalate to `architect`, or spawn per-editor children until after `0.2.0` ships.
- **Existing thin-adapter precedent:** `packages/opencode-slipfork/` (`@slipfork/opencode`, `ADR-0004`) integrates with OpenCode V2 via `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` without resolving `snow.yml` secrets or owning world state outside `slipforkd`.

# Invariants & Per-Editor UX Intent (recorded as intent, not yet a frozen spec)

1. **One control plane, N thin adapters:**
   - Editors consume a *versioned, typed* editor RPC over `slipforkd`'s existing `DaemonControl` (`<runtime_dir>/daemon.sock`, `ARCHITECTURE.md` §1, `ADR-0008` §4) — **no per-editor business logic, and no shelling out to scrape CLI text for state**.
   - One **`slipfork-mcp`** server exposing MCP tools and resources (`list_worlds`, `up`, `fork`, `restart`, `logs`, `db_query`, `open_url`, `exec`) makes **Zed** and every AI coding agent (Zed agent panel, OpenCode, Claude Code, Codex) first-class *for free*; VS Code and JetBrains layer thin native UI over the exact same protocol.
2. **Imagined per-editor depth (input to the post-`0.2.0` `docs/design/dx-editor-ide.md` blueprint):**
   - **VS Code:** worlds `TreeView` (state, origin, base key, uptime), `TaskProvider` + `TerminalProfileProvider` (`slipfork shell` as a terminal), `SimpleBrowser` + **`env.asExternalUri`** so a remote world's `demo.localhost` is clickable from a laptop without hand-rolled `ssh -L`, per-component `OutputChannel` log streaming, `DebugAdapterDescriptorFactory` attaching inside the world, and `doctor` findings surfaced as Problems with `fix:` actions.
   - **JetBrains (IntelliJ Platform):** Tool Window worlds tree, Run/Debug configurations, `ProjectOpenProcessor` to open a forked worktree, execution console per component, **HTTP Client env injection per world** (`http-client.env.json` with `BASE_URL`/`DATABASE_URL` so every `.http` request hits the right fork), and native Database tool wired via the world DSN.
   - **Zed:** MCP `context_servers` + tasks + DAP templates over `slipfork-mcp` — zero extension code, deep reach.
3. **Zero vaporware:**
   - No `docs/` page and no `README.md` claim about editor plugins until the first one ships; `docs/VISION.md` is the only home for this until then.

# Acceptance

- [ ] Post-`0.2.0` `dx-first` blueprint (`docs/design/dx-editor-ide.md`, critiqued under `dx` + `ai-ergonomics`) and `ARCHITECTURE.md` / ADR contract accepted before per-editor child nodes are created.
- [ ] `uv run --no-project tools/plan.py validate`, `make check`, and `make docs-check` exit 0 with shared conformance and protected critical coverage over the editor RPC and `slipfork-mcp`.
