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
updated: "2026-11-15T18:51"
---

# Goal

Ship CLI part 2: first-class hook and migration failure debugging, component log access, interactive shells, the `db` / `redis` / daemon-owned `forward` bridges, `auth login`, `support [--json]`, and the `git-remote-slipfork` binary, with protected critical integration tests.

# Context

- `M2.7` added streamed exec, direct world entry (`WorldEntryCommand`), and daemon-internal `dial_world_tcp`; `M2.9` added `<runtime_dir>/daemon.sock` `DaemonControl` RPC (including daemon-owned `forward`); `M2.16` wired the `slipforkd` composition root and thin-client part 1.
- The user-visible promise from `ADR-0008`: a failing hook or migration keeps the world alive, prints the full un-truncated output and the exact failing command, and offers one-line rescue commands the user can paste.
- `db` and `redis` enter the world directly from the CLI inheriting its native terminal; `forward` asks `slipforkd` to bind host `127.0.0.1:0` bridges for the lifetime of the open control connection.
- `support --json` must emit `required_evidence` from the single Rust ladder so `tools/check_support.py` consumes it rather than re-deriving tiers.

# Design / Invariants

1. **Hook failure UX (`--world <fork>`):** On non-zero hook exit, stream full untruncated stdout/stderr from `slipforkd`, print hook id (`<component>:<phase>`), exact `argv`, exit code, world id, log path, and three rescue lines (`slipfork hook run <hook_id> --world <fork>`, `slipfork logs --world <fork> <hook_id>`, `slipfork shell --world <fork>`), preserving the failed world in `slipforkd`.
2. **Direct CLI world entry — zero `SCM_RIGHTS`, zero CLI PTY (`H-010` #1):** Every `cwd`-sensitive request passes explicit `cwd: PathBuf`. For `slipfork hook run <hook_id> [--world <fork>]` (default TTY mode), `slipfork shell [--world <fork>]`, `slipfork db [<id>] [--world <fork>]`, and `slipfork redis [<id>] [--world <fork>]`, verify `stdin.is_terminal()` (failing closed without a TTY and naming `hook run --no-tty` / `slipfork exec`), request `WorldExecTarget { entry: WorldEntryCommand, env: ExecEnv, target_argv, .. }` from `<runtime_dir>/daemon.sock` (`0600`), and spawn `entry` (`nsenter` / `incus exec`) with `env_clear()` + `ExecEnv` and **native `Stdio::inherit()`** (`pdb`, `psql`, `redis-cli` inherit the caller's real terminal directly). `hook run --no-tty` runs non-interactively via `DaemonControl::hook_run`.
3. **`slipfork exec [--world <fork>] [--cwd <dir>] -- <argv>...`:** Fetches `WorldExecTarget` from `slipforkd` (sole owner of `snow.yml` `modes.local` env resolution) and executes `argv` inside the world inheriting pipe/caller stdio and propagating the child exit code.
4. **`slipfork logs [--world <fork>] [<target>] [--follow] [--stderr] [--lines <n>]`:** Passes explicit `cwd` and streams retained logs over `DaemonControl::logs`; unknown targets list valid candidates and exit non-zero.
5. **Daemon-owned `slipfork forward [--world <fork>] [--json] [<target>...]` (`ADR-0008` §4):** Sends `ForwardRequest { cwd, world, targets }` over `<runtime_dir>/daemon.sock`; **`slipforkd` binds host `127.0.0.1:0` listeners in-process** and bridges each accepted connection to `WorldEngine::dial_world_tcp` inside `slipforkd` (zero stream-upgrade, zero FD handover), replying `{world, targets:[{id, host_addr, uri}]}` which the CLI flushes immediately while holding the control connection open until signal/disconnect (which tears the daemon-side listeners down cleanly).
6. **`slipfork auth login [- | '<url>#handle=<64-hex>']`:** Reads `#handle=` URL (from `stdin` when `-`, keeping handles out of `argv`), invokes `DaemonControl::auth_login`, never logs `#handle=`, and prints full `key_id` + `backend_label()`.
7. **`slipfork support [--json]`:** Reads compile-time `SupportRegistry::embedded()` directly in the CLI, emitting `required_evidence` from `IntegrationTier::required_evidence()` under `--json` and exiting `0`.
8. **`git-remote-slipfork` binary:** Connects to `<runtime_dir>/daemon.sock` (auto-spawning `slipforkd` if absent) and drives `slipfork_vcs::remote_helper` to list/fetch `<fork>` and `<fork>/wip` refs.
9. Protected tests: `crates/slipfork/tests/critical_cli.rs` and `crates/slipfork/tests/critical_vcs_remote.rs` carry the matrix above; the guard list is extended only through the documented mechanism.

# Acceptance

```bash
# 1. Failure UX, rescue paths, logs.
cargo test -p slipfork --all-targets
cargo test -p slipfork --test critical_cli -- --nocapture hook_failure
cargo test -p slipfork --test critical_cli -- --nocapture rescue_commands
cargo test -p slipfork --test critical_cli -- --nocapture logs_follow

# 2. Daemon-owned forward bridge (teardown on disconnect) and direct clients.
cargo test -p slipfork --test critical_cli -- --nocapture forward_bridge
cargo test -p slipfork --test critical_cli -- --nocapture db_redis_exec

# 3. Auth, support, remote helper.
cargo test -p slipfork --test critical_cli -- --nocapture auth_login
cargo test -p slipfork --test critical_cli -- --nocapture support_json
cargo test -p slipfork --test critical_vcs_remote -- --nocapture git_fetch
cargo clippy -p slipfork --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a failing hook leaves the world listed by `slipfork list`, the rescue lines contain `--world <fork>` and the real hook id, `forward` bridges a real request end to end via `slipforkd`, survives a dead target, and closes its host listener on CLI disconnect, `hook run` without a TTY exits non-zero, and `support --json` contains a `required_evidence` array for every listed integration.
