---
id: M3.32.4.3
title: "Move @slipfork/opencode to slipfork status --json"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.4.1]
paths: ["packages/opencode-slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T21:01"
updated: "2026-11-19T21:01"
---

# Goal
Replace the plugin's `slipfork list` decoding with `slipfork status --json` (`{"ok":true,"worlds":[...]}`).

# Context
`packages/opencode-slipfork/src/cli.ts:16` (`list: "list"`), `:42-68` (`buildListArgs`), `:119-138` (payload decode); `coordinator.ts:69,143,227`; `index.ts:76-77`. Keep `tests/coordinator.critical.test.ts` green (protected — never weaken).

# Acceptance
- `bun test packages/opencode-slipfork/tests` exits 0.
- `! grep -rn "slipfork list" packages/opencode-slipfork/src` exits 0.
- Write NO `RESULT.md`.
