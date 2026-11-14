---
id: M1.4
title: Execute real PostgreSQL 18 + XFS FICLONE + netns + Playwright PoC benchmark
kind: task
status: done
owner: implementer
priority: 4
depends_on: [M1.3]
paths: ["poc/**", "docs/research/m1-poc-benchmark.md"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-14T22:30"
---

# Goal

Execute real PostgreSQL 18 + XFS FICLONE + netns + Playwright PoC benchmark

# Context

See `ARCHITECTURE.md` and `docs/design/m1-core-and-poc.md`.

# Acceptance

- [ ] `uv run --no-project poc/run_benchmark.py --quick-verify` exits 0.
