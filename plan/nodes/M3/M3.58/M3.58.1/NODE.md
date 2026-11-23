---
id: M3.58.1
title: "Per-step run_id logs: #id rows, logs -r <id>|last|failed, logs --runs"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/**", "specs/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T22:54"
updated: "2026-11-22T23:22"
---

# Goal
Every executed step (image built/pulled, setup, hook) mints a 6-char run_id and streams to <store>/logs/<world>/runs/<run_id>.log + .idx + index.jsonl; BootTui shows dim #id on the row; failure hint names `slipfork logs -r <id>`; `logs -r <id>|last|failed` replays; `logs --runs` lists recorded runs via the canonical table. Eliminate double ✗.

# Acceptance
- Live failing setup prints `-r <id>` and replaying it exits 0 with the real stderr.
- `logs --runs` table + `--json` purity; regression test asserts zero `✗ ✗`.
