---
id: M2.18
title: "opencode-slipfork: explicit project state, no ambient probe, live e2e against slipfork binaries"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M2.17]
paths: ["packages/opencode-slipfork/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T22:36"
---

# Goal

Finish the `@slipfork/opencode` plugin slice of the milestone: replace the hidden filesystem probe in the worktree gateway with explicit injected state, and prove the plugin end to end against the compiled `slipfork` binary (`fork`, shell routing through the world URL, `drop`).

# Context

- `M1.6` follow-up #26: the plugin decides whether a `snow.yml` exists by calling `existsSync` inside the gateway, which makes behaviour untestable without the ambient filesystem and hides configuration errors.
- The plugin talks to the daemon, never to a namespace: shell commands must route through the `WorldOrigin` URL the daemon reports, which keeps a single ingress (`H-008`).
- `M2.16`/`M2.17` shipped the CLI surface this node drives; `M2.8`/`M2.9` shipped the daemon it connects to.

# Design / Invariants

1. **Explicit state, no ambient probe.** The gateway receives a typed `SlipforkProjectState { snowFile: string | null, repoRoot: string, configError?: string }` built by an explicit loader at session start. `existsSync` disappears from the plugin sources; a missing manifest produces a structured `no-manifest` result with a message, never a silent skip.
2. **Worktree transform.** On fork: call the CLI, read the returned URL, and register the transform result `{ cwd, url, forkId }` into the session state so later hooks do not re-run discovery.
3. **Shell routing — single env owner (`ADR-0004`, `H-010` #1 & #2):** `ctx.shell.hook("create.before")` wraps commands through `slipfork exec --cwd <cwd> -- ...` and **never** resolves or injects `snow.yml` env vars in TypeScript (`slipforkd` is the sole owner of `modes.local` env resolution, including port-qualified `$BASE_URL` backed by the in-netns loopback bridge). `ctx.session.hook("context")` injects the 2-line `<context key="slipfork-world">` note naming the fork world + single `WorldOrigin` URL and forbidding direct host `:5432`/`:8000` connections.
4. **Drop.** `ctx.session.hook` teardown releases the world through the CLI, is idempotent, and never throws from a cleanup callback (it records a warning instead).
5. **No duplicated copy of the CLI contract.** Argument names and JSON field names are declared once in the plugin and validated against a fixture recorded from the real binary, so a CLI rename fails the test instead of drifting silently.
6. **Live end-to-end test (`bun test`):** `packages/opencode-slipfork/tests/e2e.critical.test.ts` drives the plugin against compiled `slipfork` + `slipforkd` (`cargo build -p slipfork --bins`) in a disk-backed XFS temp repo (never `/tmp` tmpfs), exercising zero-setup `slipforkd --listen 127.0.0.1:0` auto-spawn, implicit worktree fork (`http://<fork>.<project>.localhost:<port>`), `cwd`-routed `slipfork exec` (verifying `$BASE_URL` resolves inside the world `netns` via the loopback bridge and from the host), and idempotent session teardown `drop`.

# Acceptance

```bash
# 1. Plugin unit, contract, and live e2e critical tests (runs via bun test).
cargo build -p slipfork
make ts-test
bun test packages/opencode-slipfork/tests/e2e.critical.test.ts

# 2. The ambient probe is gone and state is injected.
! rg -q "existsSync" packages/opencode-slipfork/src
rg -q "SlipforkProjectState" packages/opencode-slipfork/src

# 3. Repo gates.
make check
```

All commands exit `0`. Adversarial assertions required: a project without a manifest yields the structured `no-manifest` result, a second `drop` for the same session is a no-op, shell commands run in the world `cwd`, and the recorded CLI fixture fails the suite when a command name changes.
