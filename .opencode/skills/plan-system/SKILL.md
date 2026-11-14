---
name: Plan System (`tools/plan.py`)
description: How to inspect and mutate the disk-backed plan tree (`plan/`), enforce disjoint active path locks, and file typed human requests (`missing-input`, `direction-challenge`, `compat-question`, `release-gate`).
---

# Plan System (`tools/plan.py`)

All work in Slipfork is tracked in `plan/` and mutated **exclusively** via `uv run --no-project tools/plan.py`. Never hand-edit managed YAML frontmatter (`status`, `attempts`, `updated`, `escalation`) or append directly to `JOURNAL.md` / `ESCALATIONS.md` / `HUMAN.md`.

## Essential Commands

```bash
# Inspect state, tree, and actionable queue
uv run --no-project tools/plan.py validate
uv run --no-project tools/plan.py state
uv run --no-project tools/plan.py tree --depth 2
uv run --no-project tools/plan.py next --limit 15
uv run --no-project tools/plan.py show M1.1 --log --result
uv run --no-project tools/plan.py stuck

# Create & manage nodes
uv run --no-project tools/plan.py --agent planner new M1 \
  --title "Implement slipfork-snow modes & !! generator" \
  --kind task --owner implementer \
  --depends "M1.1" \
  --paths "crates/slipfork-snow/**" \
  --body-file /tmp/opencode/node-body.md

# Transition status (active checks path-overlap locks automatically!)
uv run --no-project tools/plan.py --agent lead set M1.2 active
uv run --no-project tools/plan.py --agent lead bump M1.2
uv run --no-project tools/plan.py --agent lead log M1.2 "base $(git rev-parse HEAD)"
uv run --no-project tools/plan.py --agent lead set M1.2 review
uv run --no-project tools/plan.py --agent lead set M1.2 done

# Typed human inbox requests (mandatory for missing inputs, DI blind spots, compat checks, or PR gates)
uv run --no-project tools/plan.py --agent conductor human add \
  --kind direction-challenge \
  --node M1 \
  "Question summary + concrete options A/B + recommendation"
uv run --no-project tools/plan.py human open
uv run --no-project tools/plan.py human answered
uv run --no-project tools/plan.py --agent conductor human close H-001

# Escalations & autopilot
uv run --no-project tools/plan.py --agent conductor escalate M1 --to architect --reason "core DI tree design"
uv run --no-project tools/plan.py --agent conductor autopilot on --reason "resuming"
uv run --no-project tools/plan.py --agent conductor autopilot off --reason "awaiting human validation"
```

## Path-Overlap Lock Invariant

`plan.py set <id> active` compares `<id>`'s `paths` globs against every other currently `active` node. If two tasks could write to the same file (for example, root `Cargo.toml`, `Cargo.lock`, or a shared design doc), `plan.py` refuses to activate the second task until the first leaves `active`. Serialize shared-file tasks via `--depends` rather than widening globs to `**`.
