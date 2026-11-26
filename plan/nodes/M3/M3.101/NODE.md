---
id: M3.101
title: "Remove build-internal remedies from user-facing errors"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-world/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:34"
updated: "2026-11-24T11:39"
---

# Problem

`slipfork logs -r <id>` on a stale daemon printed:

```
✗ control protocol error: no recorded run ... 
next: list recorded runs with `slipfork logs --runs`
next: re-run with the `slipfork-world-init` binary from this build
```

"re-run with the `slipfork-world-init` binary from this build" is build-internal jargon and meaningless to a user (`crates/slipfork-world/src/error.rs:196`; a sibling at `bin/world_init/fault.rs:13`).

# Change

- Replace the control-protocol error's remedy with a user action: `next: slipfork down` then `slipfork up` (or restart the daemon `systemctl --user restart slipfork`), never a build artifact name.
- Audit `crates/slipfork-world/src/**` for other internal-binary/build jargon in messages that can reach the CLI (`grep -rn "from this build\|slipfork-world-init" src`), and rewrite them to user actions.
- Keep the raw cause for `-v`/logs if useful; the default must be one sentence + one actionable `next:`.

# Acceptance

- No `slipfork-world-init`/`from this build` string in a user-facing error path.
- `cargo test -p slipfork-world` and `make check` green.
- Commit `[M3.101] ...`, done, no push.
