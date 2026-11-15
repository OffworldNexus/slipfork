---
id: M1
title: "Global Target DX/DI Design & Real CoW PoC (0.1.0)"
kind: milestone
status: done
priority: 1
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: strategist
created: "2026-11-14T22:03"
updated: "2026-11-15T11:45"
---

# Goal

Deliver BOTH Slipfork's global DX-first target architecture (`docs/`, `ARCHITECTURE.md`, `ADR-0001..0007`) AND a working, un-mocked Rust + PostgreSQL 18 + XFS `FICLONE` + Linux `netns` + Playwright PoC proving concrete speed and developer-convenience gains.

# Acceptance

- [x] `make check` exits 0 (plan validate, guard, ruff, pytest tools, cargo fmt/clippy/doc/test, zensical --strict, check_docs_honesty).
- [x] `make bench` runs real PostgreSQL 18 + XFS reflink + isolated netns + warm Python runner + headless Chromium benchmark (`docs/research/m1-poc-benchmark.md`).
- [x] Human validates M1 architectural questions in `plan/HUMAN.md` (`H-001`..`H-003`, `H-005`, `H-006`, `H-008`; `H-004` & `H-009` scoped to `M2`), and owner review-gate corrections (`M1.8` + `M1.9`) are folded into code, ADRs, and embedded `support/integrations.toml`.
