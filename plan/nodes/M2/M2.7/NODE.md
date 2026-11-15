---
id: M2.7
title: "slipfork-engine: spawn/stop_service with logs, streaming exec, dial_world_tcp"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.6]
paths: ["crates/slipfork-engine/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T18:51"
---

# Goal

Complete the `WorldEngine` service-supervision, direct CLI entry, in-netns loopback bridge, and transport half of the M2 contract on `ReflinkNetnsEngine`: `spawn_service`/`stop_service` with retained logs, live-streaming `exec_in_world`, `entry_command` -> `WorldEntryCommand` for direct CLI `Stdio::inherit()` entry (zero `SCM_RIGHTS`, zero `pty.rs` `unsafe`, `H-010` #1), `current_username` (`getpwuid_r(geteuid())` in `linux_cow.rs`), the `ReflinkNetnsEngine`-owned in-netns `127.0.0.1:<ingress_port>` loopback TCP bridge (`H-010` #2), and daemon-internal `dial_world_tcp`.

# Context

- `M2.6` lands the world-spec types, redacted env, freeze lineage, and the conformance assertions; this node adds the pieces the CLI and `slipforkd` consume.
- `slipforkd` must never inspect `engine.name()`: `dial_world_tcp(&world, port) -> Result<TcpStream, EngineError>` is the only world-transport capability (`H-008`, `ADR-0005` section 2), called exclusively inside `slipforkd`.
- Per `H-010` #1 & #2, interactive TTY commands (`shell`, `db`, `redis`, `hook run`) enter the world directly from the thin `slipfork` CLI inheriting native `Stdio::inherit()` via `WorldEngine::entry_command` — **no `pty.rs` module** (`unsafe` remains confined strictly to `linux_cow.rs`, which also exposes `current_username() -> Result<String, EngineError>` via `getpwuid_r(geteuid())` with a `// SAFETY:` proof for `M2.16`'s `UserIdentity` port). Meanwhile `ReflinkNetnsEngine` owns the in-netns loopback TCP bridge booted via `WorldSpec.local_ingress_port` so `$BASE_URL` resolves identically inside the world and on the host.
- Log capture is what makes `slipfork logs [--world <fork>] [<target>]` and failure diagnostics possible, and it must survive `stop_service` so a failed world stays inspectable.

# Design / Invariants

1. **`spawn_service`** starts a world-owned process with the clean env, captures stdout and stderr into a world-scoped, size-capped log, and exposes `ServiceHandle { id, pid }` plus a `ServiceLogs` accessor (`snapshot()` and `follow(&mut dyn FnMut(StreamChunk))`); `stop_service` signals, waits with a bounded grace period, and **keeps the captured logs**.
2. **Streaming exec:** `exec_in_world(&self, world, argv, cwd, overlay_env, on_output: &mut dyn FnMut(StreamChunk))` streams interleaved stdout/stderr chunks while accumulating the exit code and the full untruncated output; a convenience wrapper without a callback returns the aggregate.
3. **Direct `entry_command` & Engine-Owned In-Netns `:80` Bridge (`H-010` #1 & #2, `ADR-0008` §3):** `entry_command(&self, world: &WorldHandle, cwd: &Path) -> Result<WorldEntryCommand, EngineError>` returns `{ program, args_prefix, cwd }` (`nsenter --target <init_pid> --user --preserve-credentials --net --mount --pid --wd=<cwd> --` on `ReflinkNetnsEngine`; `incus exec` on `IncusEngine`) so the CLI spawns interactive/non-interactive commands directly with `Stdio::inherit()` and zero `pty.rs` `unsafe`. When `WorldSpec.local_ingress_port` is `Some(port)`, `ReflinkNetnsEngine::clone_world` sets the per-netns sysctl `net.ipv4.ip_unprivileged_port_start=0` (`/proc/sys/net/ipv4/ip_unprivileged_port_start`) inside the fresh `netns` before binding `127.0.0.1:<port>` (including `:80`, probe-verified in `critical_engine.rs` by `in_netns_bridge_port_80`) and pipes accepted TCP streams to host `slipforkd`'s HTTP ingress so in-world `$BASE_URL` requests route identically to host requests.
4. **`dial_world_tcp`:** one lazily started dialer worker per world that enters the world's network namespace via `setns` and connects to `127.0.0.1:<port>`; the worker is released on `destroy_world`, a dial to a destroyed world fails closed (`EngineError::WorldGone`), and concurrent dials are serialized per world without blocking other worlds.
5. **Supervision semantics:** `spawn_service` before the world is fully up fails closed; an unexpected service exit is reported through `ServiceState` and never silently restarted; `stop_service` on an already-stopped handle is idempotent.
6. No engine-kind ladders, no ambient env reads outside the adapters.

# Acceptance

```bash
# 1. Full suites and conformance (service and transport assertions live there).
cargo test -p slipfork-engine --all-targets
cargo test -p slipfork-engine --test critical_engine -- --nocapture service
cargo test -p slipfork-engine --test critical_engine -- --nocapture dial

# 2. Unsafe stays confined to linux_cow.rs alone (zero pty.rs).
! test -f crates/slipfork-engine/src/pty.rs
cargo clippy -p slipfork-engine --all-targets -- -D warnings

# 3. Direct WorldEntryCommand & probe-verified per-netns :80 loopback bridge.
cargo test -p slipfork-engine --test critical_engine -- --nocapture entry_command
cargo test -p slipfork-engine --test critical_engine -- --nocapture in_netns_bridge_port_80

# 4. Two worlds can bind the same port while each dial reaches its own namespace.
cargo test -p slipfork-engine --test critical_engine -- --nocapture same_port
make check
```

All commands exit `0`. Adversarial assertions required: a dial after `destroy_world` fails with `WorldGone`, `stop_service` twice is idempotent, logs of a stopped service remain readable, the in-netns `:80` bridge binds unprivileged via `net.ipv4.ip_unprivileged_port_start=0`, and two worlds binding the same port stay isolated.
