---
id: M3.171
title: "Document that dev mounts expose the host checkout's modes to apps (blastit refuses group-writable trees)"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/forkfile.md, "specs/02-cli-ux.md", AGENTS.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:56"
updated: "2026-11-25T13:07"
---

# Context

With `dev`, the live checkout is bind-mounted at the image's `WORKDIR`. Apps that refuse group/other-writable trees (blastit: "/code/.venv could be replaced by another uid") then see **your host modes** — a common `umask 002` checkout (`drwxrwxr-x`) is refused even though Slipfork pins `umask 0022` for files it creates.

# Doc

- `docs/forkfile.md` (dev section): one short paragraph + a copy-pasteable remedy — the dev mount exposes host ownership/modes; apps with strict writability checks need your checkout at `0755`/`0644` (`chmod -R g-w,o-w .` or a `0022` umask). Note that Slipfork's own container-created files are `umask 0022`.
- `specs/02` only if it describes the dev overlay's permission semantics; otherwise skip.

No code, no warning banner (an app-specific policy is not ours to police).

# Acceptance

- Paragraph fits the DX rules (≤2 sentences + one snippet), `guard.py`/`check_docs_honesty.py` green; commit `[M3.171] ...`, done, no push.
