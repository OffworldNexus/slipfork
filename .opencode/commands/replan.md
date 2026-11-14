---
description: Escalate a subtree or scope change to strategist/architect for upward replanning
agent: conductor
---

Escalate the requested scope change or stuck subtree via `uv run --no-project tools/plan.py --agent conductor escalate <node_or_milestone> --to strategist --reason "$ARGUMENTS"` and spawn `strategist` (with `architect` if DI/trait boundaries change) to update `ROADMAP.md`, supersede obsolete nodes cleanly (zero pre-1.0 fallback cruft!), and resume.
