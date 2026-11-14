---
id: M1
title: "Global Target DX/DI Design & Real CoW PoC (0.1.0)"
kind: milestone
status: ready
priority: 1
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-14T22:03"
---

# Goal

Deliver BOTH Slipfork's global DX-first target architecture (`docs/`, `ARCHITECTURE.md`, `ADR-0001..0004`) AND a working, un-mocked Rust + PostgreSQL 18 + XFS `FICLONE` + Linux `netns` + Playwright PoC proving concrete speed and developer-convenience gains.

# Acceptance

- [ ] `make check` exits 0 (plan validate, guard, ruff, pytest tools, cargo fmt/clippy/doc/test, zensical --strict).
- [ ] `make bench` runs real PostgreSQL 18 + XFS reflink + isolated netns + warm Zygote + headless Chromium benchmark and writes `docs/research/m1-poc-benchmark.md`.
- [ ] Human validates open architectural questions in `plan/HUMAN.md` (`H-001`..`H-004`).
