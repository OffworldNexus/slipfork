---
id: M3.28.19
title: OSPP v1 protocol draft for Kerfufoo integration (0.2.0)
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/design/ospp.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-17T21:44"
updated: "2026-11-17T21:48"
---

# Goal

Land the architect's OSPP v1 protocol draft (`docs/design/ospp.md`, 166 lines, 2 `sequenceDiagram`s) for the `0.2.0` Kerfufoo provider integration. It is a **target spec**, deliberately newer than the shipped code; the job is to verify and fix factual drift, not to make it match `0.1.0`.

# Context

The architect drafted this file this session from the owner's framing (cloud-in-a-box; the local world fetches the same secrets/values CI has). It specifies a newer protocol shape on purpose:

- negotiated auth methods in `hello` / `auth_challenge` / `auth_proof`,
- project identity = repo remote + `snow.yml` digest,
- `resolve` instead of today's `env.bundle`.

Those differences are intended and stated in the doc. The shipped client (`crates/slipfork-ospp/src/{protocol,client}.rs`) speaks `challenge` / `hello` / `welcome` / `reject`, keyed on `project` + `mode`, with `env.bundle` / `secrets.ensure` / `resources.describe` / `dump.begin..dump.end`. Do not rewrite the draft into that older shape.

# Design / Invariants

1. Verify against the code for **factual drift only**: claims the draft presents as *shipped today* must be true of `0.1.0`; claims presented as the `0.2.0` target stay as written. Anything ambiguous gets a sentence that names the version, never a silent rewrite.
2. Keep the file lean and scannable; keep its 2 `sequenceDiagram`s (they are the draft's whole point). Do not add history, trade-off essays, or plan ids.
3. Fix only: wrong file/API names presented as current, "ships today" claims that are false, broken internal links, and statements that contradict `docs/reference/ospp.md` (which says the wire is unpublished and no provider ships).
4. No code changes.

# Acceptance

```console
$ uvx --offline zensical build --strict
... exit 0
$ uv run --no-project tools/guard.py
guard: OK
$ uv run --no-project tools/plan.py validate
plan OK
$ test -f docs/design/ospp.md && grep -c '```mermaid' docs/design/ospp.md
2
$ ! grep -rniE 'ships? today|already implemented|is implemented in 0\.1\.0' docs/design/ospp.md
$ git status --short docs/design/ospp.md
?? docs/design/ospp.md
```

`RESULT.md` lists every drift item found and whether it was fixed or left as an intentional `0.2.0` difference.
