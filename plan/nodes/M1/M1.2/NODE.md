---
id: M1.2
title: "Design global IoC/DI architecture, crate seams, and ADR-0001..0004"
kind: task
status: done
owner: architect
priority: 2
depends_on: [M1.1]
paths: [ARCHITECTURE.md, docs/VISION.md, "docs/design/m1-core-and-poc.md", "docs/decisions/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-14T22:30"
---

# Goal

Design global IoC/DI architecture, crate seams, and ADR-0001..0004

# Context

See `ARCHITECTURE.md` and `docs/design/m1-core-and-poc.md`.

# Acceptance

- [ ] `uv run --no-project tools/guard.py` exits 0.
