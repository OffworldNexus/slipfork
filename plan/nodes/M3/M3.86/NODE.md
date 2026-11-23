---
id: M3.86
title: "Update the opencode plugin e2e drop refusal for the new no-fork message"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["packages/opencode-slipfork/tests/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:36"
updated: "2026-11-24T00:38"
---

# Problem

`make check` -> `js-test` fails at `packages/opencode-slipfork/tests/e2e.test.ts:180`:

```
Expected to contain: "is not a Btrfs subvolume"
Received: "slipfork drop 'feat' failed: no fork named `feat` exists\nnext: run `slipfork ps` to list this world's forks"
```

`M3.80` removed the up-time adoption and the "not a Btrfs subvolume" drop precondition; `drop` now reports a missing fork from the registry with `next: slipfork ps`.

# Change

- In the real-CLI journey test, stop asserting the old string. Derive the expected drop refusal from the binary the same way the fork case already does: run `slipfork drop feat --json` in `probe.project`, `decodeCliFailure` it, and assert `dropError.message === \`slipfork drop 'feat' failed: ${decoded}\``.
- Keep the rest of the journey untouched (fork refusal parity, listing, shell exports, reaping).

# Acceptance

- `make check` green (or at least `make js-test` green) and `bun test` for the plugin passes.
- Commit `[M3.86] ...`, done, no push.
