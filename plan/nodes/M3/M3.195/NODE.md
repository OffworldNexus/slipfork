---
id: M3.195
title: "docs/tutorial: declare the front port unconditionally (no on-dev port override)"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/tutorial.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-26T07:26"
updated: "2026-11-26T07:26"
---

# Problem

The tutorial's SvelteKit step declares `port 3000` unconditionally and then `on dev { port 5173; run: … }`. `specs/01:90` says a `port` inside `on dev` never satisfies `web.port`, and nothing proves it overrides the listening port at runtime either. A tutorial must not hinge on an unproven override.

# Fix

- Declare the dev port **unconditionally** (`port 5173`) and keep only the `run:` under `on dev` (or make both stages use the same port).
- If the override is genuinely supported and you can point to the code/test that proves it, keep it and add a one-line note; otherwise drop it.
- Re-run `make docs-check`.

# Acceptance

- `docs/tutorial.md` no longer depends on a `port` declared inside `on dev`.
- `make docs-check` green; commit `[M3.195] ...` with `docs/tutorial.md` only, done, no push.
