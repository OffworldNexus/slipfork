---
id: M3.32.9
title: "Migrate docs: docs/reference/cli.md, getting-started, CHANGELOG + legacy demo migration rehearsal"
kind: task
status: done
owner: "doc-writer"
priority: 2
depends_on: [M3.32.5, M3.32.8]
paths:
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - "support/demo/**"
  - zensical.toml
  - tools/check_docs_honesty.py
  - "crates/slipfork-forkfile/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T16:00"
updated: "2026-11-19T22:27"
---

# Goal

Document and rehearse the one-command migration path.

# Context

OWNER ADDITION to `M3.32`. Published docs must describe only what the binary does.

- `docs/reference/cli.md`: add `slipfork migrate` with a copy-pasteable example in the first lines of the section.
- `docs/getting-started.md`: a "Coming from snow.yml?" admonition with `slipfork migrate` and the one canonical-runtime sentence (runtime reads only `Forkfile`).
- `CHANGELOG.md`: `0.1.0` entry mentions `slipfork migrate`.
- `support/demo/`: keep a legacy-shaped `snow.yml` fixture (or a `migrate/` sample) so the conversion is exercised by a real file, while the runnable demo uses `Forkfile`.

# Acceptance

- `uvx --offline zensical build --strict`, `uv run --no-project tools/check_docs_honesty.py`, `make docs-check` exit 0.
- No published claim mentions `slipfork list`, `snow resolve`, or a dual runtime.
- Write NO `RESULT.md`.
