---
id: M3.114
title: "up --no-cache: bust the image cache and force a rebuild"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109]
paths:
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/world/**"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-image/src/steps.rs"
  - "crates/slipfork-image/src/builder/**"
  - "crates/slipfork-image/src/lib.rs"
  - docs/cli.md
  - "specs/02-cli-ux.md"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T16:16"
updated: "2026-11-24T20:28"
---

# Problem

`slipfork up` has no way to ignore the image cache. Images are reused from sealed stamps keyed by the Forkfile/context digest and the run fingerprint, so a stale or suspect image cannot be forced to rebuild from the CLI (Docker has `--no-cache`).

# Contract

- `slipfork up [TARGET] --no-cache`: force every image in the world to rebuild — ignore sealed image stamps/cache hits, re-run the stage walk, re-seal — then boot/reconcile. It must not be a no-op `Idle`: pass `no_cache` through the daemon request and force `Reconcile`/`Boot` even when the Forkfile fingerprint is unchanged.
- `slipfork docker build <component> --no-cache` (the M3.109 verb) uses the same path.
- Without the flag, behaviour is unchanged (`cached`/`up to date`, zero extra runs).
- `--json` carries a boolean; help/docs/spec describe it; one `next:` on failure.

# Acceptance

- Live: `up` (cached) → step rows say `cached`/`<1ms`; `up --no-cache` → every image step executes (new run ids, real timings) and the world boots; a following plain `up` is cached again.
- `make check`, targeted live journey; commit `[M3.114] ...`, done, no push.
