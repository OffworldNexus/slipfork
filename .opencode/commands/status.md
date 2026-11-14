---
description: Show concise Slipfork plan progress, active nodes, open HUMAN items, and budget
agent: conductor
---

Run `uv run --no-project tools/plan.py state` and `uv run --no-project tools/plan.py tree --depth 2`. Summarize milestone progress, open `plan/HUMAN.md` items, and expensive-tier budget in a compact table without starting new background work unless asked.
