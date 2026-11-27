---
id: M3.162
title: Document the container umask guarantee in specs/03 and docs
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

# Goal

`M3.160` shipped a single `PROCESS_UMASK = 0o022` in `slipfork-world::identity`, rendered as OCI `process.umask` for both build steps and pods (never an env var), so containers deterministically create `0755` dirs and `0644` files regardless of the daemon's inherited umask.

Document the guarantee (non-blocking follow-up from its Triad): one short paragraph in `specs/03` (the identity/§6 area) — build steps and service pods run with `umask 0022`; generated trees (`.venv`, wheels) are not group-writable; ownership still maps to the invoking uid on the host. One line in `docs/forkfile.md` if any example depends on group-writable output.

# Acceptance

- `specs/03` states the umask rule once, in ≤2 sentences, with no ADR prose; `docs/forkfile.md` consistent.
- `guard.py` and `check_docs_honesty.py` green; commit `[M3.161] ...`, done, no push.
