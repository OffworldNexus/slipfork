---
id: M3.104
title: One sentence + one next in JSON error paths (no embedded remedy duplication)
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/commands/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:42"
updated: "2026-11-24T11:45"
---

# Problem

`M3.102` added the typed `daemon-skew` error whose message contains `\nnext: …`. `commands/mod.rs::report` and `commands/streams.rs::existing_daemon` don't split an embedded remedy, so `env/secret/logs --json` emit `{"error":{"message":"…\nnext: …","next":"generic"}}` — the same remedy twice, and the generic one is wrong. `status --json` still renders plain text for this path.

# Change

- Add one helper that splits a daemon message at its trailing `\nnext: …` into `(message, next)`; use it in `report`/`existing_daemon` (and anywhere else that builds the JSON error envelope).
- `--json` carries a clean `message` and exactly the daemon's own `next` field; human output prints one sentence + one `next:` line.
- Make `status --json` honour `--json` for daemon errors (no plain text).
- Do not change error codes.

# Acceptance

- Real-lab: start a stale daemon (`daemon_skew_live` fixture), run `env --json`/`logs --json`/`status --json`; assert pure JSON, one `message`, one `next` equal to the daemon's remedy, no embedded `next:` text.
- `make check`; commit `[M3.104] ...`, done, no push.
