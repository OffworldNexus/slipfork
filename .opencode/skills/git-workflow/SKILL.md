---
name: Git & Release PR Workflow
description: Rules for explicit file staging, pre-commit checks, release/<version> branch hygiene, and human-merged Release PRs into develop.
---

# Git & Release PR Workflow

1. **Who Commits:** Only `lead` and `conductor` run `git add` and `git commit`. Leaf workers (`implementer`, `designer`, `doc-writer`, `researcher`, `expert`) never commit.
2. **Explicit Staging Only (Never Broad Globs):**
   - Inspect `git status --short` first.
   - Stage **only** the exact files belonging to the completed node (`git add -- crates/slipfork-snow/src/lib.rs plan/...`), never `git add .`, `git add -A`, or broad directory globs that could sweep another active agent's untracked file.
3. **Mandatory Pre-Commit Gate:**
   Before every commit, run:
   ```bash
   uv run --no-project tools/plan.py validate
   uv run --no-project tools/guard.py
   uv run --no-project tools/critical_guard.py --base <base_sha> --node <ID>
   make check
   ```
   All four must exit `0`.
4. **Commit Message Format:**
   - Plan node completion: `[<ID>] <concise imperative summary>` (verified against `git diff --cached --stat`).
   - Plan-only state sync: `[conductor] <summary>`.
5. **Branch & Release Discipline:**
   - Work on `release/<version>` branches cut from `develop` (e.g. `release/0.1.0`).
   - Never push directly to `develop`, never force-push, never create git tags, and never run `gh pr merge`. When a milestone/release gate passes, `conductor` pushes `release/<version>`, opens a PR into `develop`, files a `release-gate` item in `plan/HUMAN.md`, turns `autopilot` `off`, and lets the human review and merge.
