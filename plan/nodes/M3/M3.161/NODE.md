---
id: M3.161
title: Document the container umask guarantee in specs/03 (§8) and docs
kind: task
status: superseded
owner: "doc-writer"
priority: 3
depends_on: []
paths: ["specs/03-under-the-hood.md", docs/forkfile.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T12:28"
updated: "2026-11-25T12:29"
---

`M3.160` shipped a single `PROCESS_UMASK = 0o022` in `slipfork-world::identity`, rendered as OCI `process.umask` for both build steps and pods (never an env var), so containers deterministically create `0755` dirs and `0644` files regardless of the daemon's inherited umask.

Document the guarantee (non-blocking follow-up from its Triad):
- `specs/03` §8 (or the identity section): one short paragraph — build steps and service pods run with `umask 0022`; app `.venv`/generated trees are not group-writable; ownership still maps to the invoking uid on the host.
- `docs/forkfile.md`: if any example relies on writable-by-group behaviour, note the umask rule in one line.

Keep DX rules (≤2-sentence paragraphs, no ADR prose); `guard.py` + `check_docs_honesty.py` green; commit `[M3.161] ...`, done, no push.
