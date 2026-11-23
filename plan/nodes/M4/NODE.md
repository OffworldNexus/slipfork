---
id: M4
title: "Dedicated Polyglot Runner Plugins, Remote CI Snapshot/Artifact Cache & Agent Ecosystem (0.4.0)"
kind: milestone
status: ready
priority: 4
depends_on: [M3]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-16T17:25"
---

# Goal

Deliver Slipfork `0.4.0`: extended `STP v1` language plugins (`Playwright`, `Pest`, `cargo nextest`), Blastit parallel wire learning over `SLIPFORK_OUTPUT_DIR`, content-addressed remote CI/team Btrfs snapshot & artifact cache export/import, `llms.txt` + turnkey agent skills (OpenCode, Claude Code, Codex, Junie), and the isolated `WeavedRegistrar` IPC adapter (`M4.1`, gated on `H-006`).

# Acceptance

- [ ] `make check` exits 0 and milestone exit criteria in `plan/ROADMAP.md` verified.
