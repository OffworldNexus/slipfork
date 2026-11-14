---
description: Process newly answered items in plan/HUMAN.md and resume work
agent: conductor
---

Check `uv run --no-project tools/plan.py human answered` and read `plan/HUMAN.md`. Apply every human answer (unblocking nodes or dispatching `strategist`/`architect`/`planner` if direction changed), close answered items, turn autopilot `on` if actionable work exists, and resume the Main loop.
