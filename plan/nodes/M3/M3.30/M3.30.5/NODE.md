---
id: M3.30.5
title: "Visual high-hierarchy 0.1.0 docs: value-first quickstart, Mermaid diagram, admonitions & single CHANGELOG entry"
kind: task
status: ready
owner: "doc-writer"
priority: 3
depends_on: [M3.30.4]
paths: [README.md, CHANGELOG.md, zensical.toml, "docs/**", support/demo/README.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-18T22:19"
---

# Goal
Docs that sell the product in the first 25 lines and stay scannable: value-first headline, copy-pasteable quickstart, a real Mermaid workflow diagram, admonitions for prerequisites, no paragraph walls.

# Context
Audit-verified state (HEAD).
- Rewrite: `README.md`, `docs/index.md`, `docs/getting-started.md`, `docs/snow-modes.md`, `docs/agents-and-opencode.md`, `docs/reference/cli.md`, `docs/reference/manifest.md`, `docs/reference/ospp.md`, `docs/design/do-vm-harness.md`, `support/demo/README.md`.
- `CHANGELOG.md`: collapse to one clean `0.1.0` release entry (no archaeology, no "Option A vs B").
- **0 Mermaid diagrams exist across `docs/` today** — add at least one crisp workflow diagram (manifest → world → worktree fork → `.localhost` URL → OSPP) and keep the rest of the site's diagrams consistent.
- `docs/getting-started.md` and `docs/reference/cli.md` contain `653`-char and `500`-char paragraph walls — every paragraph must be `<= 280` chars.
- Docs must describe only what ships in the binary (`tools/check_docs_honesty.py`); pre-code blueprints stay in `docs/design/dx-*.md`.
- No internal `ADR-*` / `M3.*` lawyer prose in public pages.

# Deliverables
- `README.md` and `docs/index.md` lead with **"Slipfork runs your cloud environment locally in a box; CI is the clutch"**, then a 10-line quickstart (`sudo slipfork daemon install` → `slipfork up` → open `http://<mode>.localhost`) inside the first 25 lines.
- A working copy-pasteable snippet in the first 25 lines of every getting-started-style page.
- Zensical `!!! info` / `!!! warning` admonitions for Btrfs, AppArmor, and `daemon install` prerequisites.
- `http://demo.localhost` written without `:80`; digest/URL styling consistent with the CLI.
- `docs/reference/cli.md` regenerated to match the post-sweep command surface (no `support` command, no `dag plan`).

# Constraints
- Public docs = shipped behavior only. No vaporware, no future-tense features.
- Explicit file staging by the lead only; this node writes docs, not code.
- `<= 2`-sentence paragraphs; no walls.

# Acceptance
- `uvx --offline zensical build --strict`
- `uv run --no-project tools/check_docs_honesty.py`
- `make docs-check`
- `grep -c '```mermaid' docs/**/*.md | grep -v ':0'` shows at least one Mermaid diagram.
- `awk 'length > 280 {print FILENAME": "FNR}' docs/getting-started.md docs/reference/cli.md README.md` returns nothing (outside code fences).
- `! grep -rn "slipfork dag plan\|slipfork support" docs/ README.md`
- `! grep -rn "ADR-[0-9]\|M3\.[0-9]" README.md docs/index.md docs/getting-started.md`
