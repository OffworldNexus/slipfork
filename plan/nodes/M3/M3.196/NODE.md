---
id: M3.196
title: "Tutorial: explain that one world domain means no CORS between front and API"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/tutorial.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-26T07:30"
updated: "2026-11-26T07:30"
---

# Goal

The tutorial's step 3 (SvelteKit front) shows `route /` on the front and `route /api` on the API but never says why that's better than two ports. Add the CORS rationale.

# Change

- After the "longest prefix wins, both answer on `http://hello.localhost`" line in `docs/tutorial.md`, add **at most two sentences**: because both services share one world domain (one origin), the browser calls `/api` with no cross-origin request, so no CORS config, no proxy and no absolute API URL are needed — and cookies/sessions work as usual.
- Optional single `!!! tip` admonition if it reads better than prose, but keep the step ≤40 lines and the page lean.
- Do not invent a second snippet, and do not suggest absolute URLs.

# Acceptance

- `make docs-check` green; step 3 stays ≤40 lines; commit `[M3.196] ...` with `docs/tutorial.md` only, done, no push.
