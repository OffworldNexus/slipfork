---
description: Pause the Slipfork autopilot loop cleanly
agent: conductor
---

Run `uv run --no-project tools/plan.py --agent conductor autopilot off --reason "paused by user"`, regenerate `uv run --no-project tools/plan.py state`, and report a concise status summary.
