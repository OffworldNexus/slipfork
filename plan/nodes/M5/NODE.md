---
id: M5
title: "Remote Snapshot/Artifact Cache, slipfork-mcp Agent Server & Thin IDE Integrations (VS Code, JetBrains, Zed) (0.3.0)"
kind: milestone
status: ready
priority: 5
depends_on: [M4]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T17:47"
updated: "2026-11-25T19:21"
---

# Goal

Deliver Slipfork **`0.3.0`**: content-addressed remote `SnapshotStore` cache export/import, `llms.txt` + coding-agent skills, and thin native IDE integrations (**VS Code**, **JetBrains**, **Zed**) over one shared daemon RPC + `slipfork-mcp` server, scheduled strictly after `M4` (`0.2.0` `slipfork test`).

# Context & Architectural Invariants

- **Dependency honesty:** IDE integrations ship only after `M4` (`0.2.0` `slipfork test` + `OSPP v1` pairing) so worlds, logs, per-world test runs, and coverage share one typed protocol.
- **One control plane, N thin adapters:** Editors and agents consume a versioned typed RPC on `<runtime_dir>/daemon.sock` plus one `slipfork-mcp` server (`list_worlds`, `up`, `fork`, `drop`, `restart`, `logs`, `exec`, `test`) — zero per-editor business logic and zero CLI text scraping.
- **Specs before code & zero vaporware:** Draft the `specs/` contract before decomposing per-editor slices; publish zero `docs/` or `README.md` editor claims until shipped and verified.

# Acceptance

- [ ] Remote `SnapshotStore` cache export/import, `slipfork-mcp`, and thin editor adapters pass `make check`, `make live-test`, `make docs-check`, `tools/guard.py`, and `tools/plan.py validate`.
