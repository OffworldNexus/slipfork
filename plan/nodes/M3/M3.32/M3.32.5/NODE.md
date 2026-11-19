---
id: M3.32.5
title: "Forkfile visual docs, CHANGELOG & multi-service (api + front + postgres) support/demo"
kind: task
status: ready
owner: "doc-writer"
priority: 3
depends_on: [M3.32.4]
paths:
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - AGENTS.md
  - "support/demo/**"
  - "poc/**"
  - zensical.toml
  - tools/check_docs_honesty.py
  - "crates/slipfork-forkfile/tests/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T13:03"
---

# Goal

Make the shipped surface legible: one real multi-service `Forkfile` demo and problem-first visual docs for `Forkfile`/CLI, with dead `snow.yml` docs deleted and every published claim matching the binary.

# Context

- `support/demo` today is single-service: `support/demo/snow.yml`, `support/demo/slipfork.toml`, `support/demo/Dockerfile`, `support/demo/app/server.py`, `support/demo/db/000{1,2}_*.sql`. Replace with a `Forkfile`:

  ```text
  postgres pg { database demo }
  service api   ./api   { port 8000; route /api; volume .venv; task migrate in api: ... }
  service front ./front { port 5173; route / }
  ```

  so `http://demo.localhost/` and `http://demo.localhost/api` are served by **separate** `crun` containers, `pg` is a third, and `api` exposes a `task` invoked by `slipfork run`. Delete `snow.yml`/`slipfork.toml` from the demo; keep the seed SQL.
- Docs to (re)write: new `docs/forkfile.md` + `docs/reference/forkfile.md` (syntax tour, first working snippet in the first 25 lines, Mermaid flow, `!!!` admonitions, `<= 2`-sentence paragraphs); `docs/reference/cli.md` (from `dx-cli-0.1.0.md` §3 table, delete `list`/`snow resolve` rows); `docs/getting-started.md`; `docs/index.md`; `docs/agents-and-opencode.md` (`status --json`); `docs/reference/ospp.md`; `README.md`; `CHANGELOG.md` (single `0.1.0` entry); `AGENTS.md` (Forkfile + per-service `crun` reality).
- **Delete** `docs/snow-modes.md` and `docs/reference/manifest.md`; update `zensical.toml` nav accordingly.
- `tools/check_docs_honesty.py` may need the new terms allowlisted so it stays green; `poc/**` may need `Forkfile`-shaped fixtures if it still references `snow.yml`. No `ADR-*` lawyer prose in published docs.

# Acceptance

- `uvx --offline zensical build --strict` exits `0`.
- `uv run --no-project tools/check_docs_honesty.py` and `make docs-check` exit `0`.
- `! grep -rn 'snow.yml\|slipfork.toml\|slipfork list\|snow resolve' README.md docs/ support/demo/ zensical.toml` exits `0`.
- `support/demo/Forkfile` parses through `slipfork-forkfile` in a test (`cargo test -p slipfork-forkfile`) with 3 services and 1 postgres database, and a demo rehearsal `slipfork up` transcript is captured in the node `LOG.md`.
- Write NO `RESULT.md`.
