---
id: M3.154
title: "Build args: bind services (port) in the arg symbol table and fail closed when an arg does not evaluate"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/src/eval/**"
  - "crates/slipfork-forkfile/src/tests/**"
  - "crates/slipfork-forkfile/src/check/**"
  - "crates/slipfork-image/src/inputs.rs"
  - "crates/slipfork-image/tests/**"
  - docs/forkfile.md
  - "specs/01-forkfile.md"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T10:13"
updated: "2026-11-25T12:43"
---

# Problem (owner question: "in a forkfile how do I pass an arg to the dockerfile? … like the port")

`arg KEY = <expr>` exists, but `BoundSymbol` (`crates/slipfork-forkfile/src/eval/values.rs:11-30`) only binds **resources and `self`** (postgres/database/redis/s3/cdn/domain). Services are not bound, so the natural `arg PORT = web.port` evaluates to `None`, and `collect_image_args` (`eval/mod.rs:238-243`) **silently drops** the binding — the Dockerfile then sees no `ARG PORT` value, with no diagnostic. That is a trap, not a feature gap.

# Fix

1. Bind every `service` (and every routed app) as a symbol with the properties it actually has: `port` (its declared `port <u16>`), and `name`; keep `self.*` and the resource symbols unchanged. Do not invent a per-service URL — routing is by world/domain or `route … domain <id>`.
2. **Fail closed**: an `arg` whose expression does not evaluate is a diagnostic naming the arg, the expression and the valid properties (one line + a `next:` suggestion), instead of silently omitting the build arg. Same for `on` branches.
3. Document the full property table in `docs/forkfile.md` (one small table) and `specs/01` §2, with a copy-pasteable example:

```
image api . {
  arg DB_URL = db.url
  arg REDIS_PORT = cache.port
  arg PORT = web.port
  on dev { arg DEBUG = "1" }
}
```

# Acceptance

- Unit tests: `arg PORT = web.port` yields the declared port; `arg X = web.nope` fails with the diagnostic; an unknown base (`arg X = ghost.port`) fails with the diagnostic; a resource arg still evaluates; an `on dev`-only arg appears only under `dev`.
- The built image receives the value (`docker build`/`up` shows the ARG resolved in the step log); the image digest changes when the port changes.
- `make check`; Triad; commit `[M3.154] ...`, done, no push.
