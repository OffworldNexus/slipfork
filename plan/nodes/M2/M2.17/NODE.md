---
id: M2.17
title: "CLI part 2: hook failure rescue UX, logs/shell/db/redis/forward, auth, support, git-remote-slipfork"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.16]
paths: ["crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Ship CLI part 2: first-class hook and migration failure debugging, component log access, interactive shells, the `db` / `redis` / `forward` bridges, `auth login`, `support [--json]`, and the `git-remote-slipfork` binary, with protected critical integration tests.

# Context

- `M2.7` added streamed and interactive-TTY exec plus `dial_world_tcp`; `M2.9` added the control IPC; `M2.16` wired the composition root and part 1.
- The user-visible promise from `ADR-0008`: a failing hook or migration keeps the world alive, prints the full un-truncated output and the exact failing command, and offers one-line rescue commands the user can paste.
- `db` and `redis` are thin TTY clients executed **inside** the world; `forward` is a host-side TCP bridge that never assumes a fixed port is free.
- `support --json` must emit `required_evidence` from the single Rust ladder so `tools/check_support.py` consumes it rather than re-deriving tiers.

# Design / Invariants

1. **Hook failure UX.** On a non-zero hook exit: stream stdout and stderr live to a TTY (buffered but complete when not a TTY), then print the failing hook id, the exact argv, the exit code, the world id, the resolved log path, and exactly three rescue lines (`slipfork hook run <hook_id> --world <fork>`, `slipfork logs <fork> <hook_id>`, `slipfork shell <fork>`). The world is never destroyed on failure. Output is never truncated in the failure summary.
2. **`slipfork hook run <hook_id> [--world <fork>]`.** Re-executes one hook inside the existing world with the same environment overlay; interactive TTY stdio is attached so a debugger breakpoint works; a non-interactive stdin is a hard error with an actionable message rather than a silent hang.
3. **`slipfork logs [<fork>] [<component_or_hook>]`.** Reads captured logs from the engine, `--follow` streams, `--stderr` filters, an unknown filter lists the available names instead of printing nothing.
4. **`slipfork shell [<fork>]`.** Interactive TTY exec as the world user in the world cwd; no TTY means a fail-closed error.
5. **`slipfork db [<id>] [--world <fork>]` and `slipfork redis [<id>] [--world <fork>]`.** Resolve the target through the manifest, exec the real client inside the world, inherit the TTY, forward the client exit code, and refuse an ambiguous multi-target selection by listing the candidates.
6. **`slipfork forward [--world <fork>] [<target>...]`.** Binds `127.0.0.1:0` on the host, accepts connections, and bridges each one through `dial_world_tcp` (never a raw namespace assumption); prints the bound address and ready-to-paste GUI URIs, supports graceful shutdown on signal, and reports a target that is not listening inside the world without crashing the bridge.
7. **`slipfork auth login`.** Runs the pairing flow: opens the URL with the fragment handle, never logs the handle, stores the resulting device identity through the injected `DeviceKeySigner` path, and prints a one-line confirmation.
8. **`slipfork support [--json]`.** Human table by default; `--json` emits tiers plus `required_evidence` per integration from the Rust ladder only. Exit `0` even when integrations are below `declared` (it reports, it does not gate), while `tools/check_support.py` remains the gate.
9. **`git-remote-slipfork` binary.** Wired from the crate root to the `VcsWorkspace` layer, so `git fetch slipfork` materializes and reports fork refs; a repository without Slipfork state fails with a clean protocol error.
10. Protected tests: `crates/slipfork/tests/critical_cli.rs` and `crates/slipfork/tests/critical_vcs_remote.rs` carry the matrix above; the guard list is extended only through the documented mechanism.

# Acceptance

```bash
# 1. Failure UX, rescue paths, logs.
cargo test -p slipfork --all-targets
cargo test -p slipfork --test critical_cli -- --nocapture hook_failure
cargo test -p slipfork --test critical_cli -- --nocapture rescue_commands
cargo test -p slipfork --test critical_cli -- --nocapture logs_follow

# 2. Bridges and clients.
cargo test -p slipfork --test critical_cli -- --nocapture forward_bridge
cargo test -p slipfork --test critical_cli -- --nocapture db_redis_exec

# 3. Auth, support, remote helper.
cargo test -p slipfork --test critical_cli -- --nocapture auth_login
cargo test -p slipfork --test critical_cli -- --nocapture support_json
cargo test -p slipfork --test critical_vcs_remote -- --nocapture git_fetch
cargo clippy -p slipfork --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a failing hook leaves the world listed by `slipfork list`, the rescue lines contain the real fork and hook ids, `forward` bridges a real request end to end and survives a dead target, `hook run` without a TTY exits non-zero, and `support --json` contains a `required_evidence` array for every listed integration.
