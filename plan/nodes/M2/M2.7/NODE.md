---
id: M2.7
title: "slipfork-engine: spawn/stop_service with logs, streaming and PTY exec, dial_world_tcp"
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
updated: "2026-11-15T16:40"
---

# Goal

Complete the `WorldEngine` service-supervision and transport half of the M2 contract on `ReflinkNetnsEngine`: `spawn_service`/`stop_service` with captured logs, live-streaming world exec, interactive-TTY world exec (for a debugger inside `slipfork hook run` and `slipfork shell`), and `dial_world_tcp` through a per-world network-namespace dialer worker.

# Context

- `M2.6` lands the world-spec types, redacted env, freeze lineage, and the conformance assertions; this node adds the pieces the CLI and `slipforkd` consume.
- `slipforkd` must never inspect `engine.name()`: `dial_world_tcp(&world, port) -> Result<TcpStream, EngineError>` is the only world-transport capability (`H-008`, `ADR-0005` section 2).
- An interactive terminal needs a pseudo-terminal, which requires one small `unsafe` module (open/grant/unlock the ptmx plus the window-size ioctl). `ADR-0008` extends the crate's unsafe policy table to exactly that module.
- Log capture is what makes `slipfork logs [<fork>] [<component_or_hook>]` and failure diagnostics possible, and it must survive `stop_service` so a failed world stays inspectable.

# Design / Invariants

1. **`spawn_service`** starts a world-owned process with the clean env, captures stdout and stderr into a world-scoped, size-capped log, and exposes `ServiceHandle { id, pid }` plus a `ServiceLogs` accessor (`snapshot()` and `follow(&mut dyn FnMut(StreamChunk))`); `stop_service` signals, waits with a bounded grace period, and **keeps the captured logs**.
2. **Streaming exec:** `exec_in_world(&self, world, argv, cwd, overlay_env, on_output: &mut dyn FnMut(StreamChunk))` streams interleaved stdout/stderr chunks while accumulating the exit code and the full untruncated output; a convenience wrapper without a callback returns the aggregate.
3. **Interactive TTY exec:** `exec_interactive_in_world(...) -> Result<ExitStatus, EngineError>` allocates a pseudo-terminal, mirrors stdin/stdout/stderr one-to-one (raw mode plus window-size propagation), and restores the host terminal on every exit path including panic. The unsafe terminal calls live in one module with per-call SAFETY proofs; no other file gains unsafe.
4. **`dial_world_tcp`:** one lazily started dialer worker per world that enters the world's network namespace via `setns` and connects to `127.0.0.1:<port>`; the worker is released on `destroy_world`, a dial to a destroyed world fails closed (`EngineError::WorldGone`), and concurrent dials are serialized per world without blocking other worlds.
5. **Supervision semantics:** `spawn_service` before the world is fully up fails closed; an unexpected service exit is reported through `ServiceState` and never silently restarted; `stop_service` on an already-stopped handle is idempotent.
6. No engine-kind ladders, no ambient env reads outside the adapters.

# Acceptance

```bash
# 1. Full suites and conformance (service and transport assertions live there).
cargo test -p slipfork-engine --all-targets
cargo test -p slipfork-engine --test critical_engine -- --nocapture service
cargo test -p slipfork-engine --test critical_engine -- --nocapture dial

# 2. Unsafe stays confined to the two proven files.
test "$(rg -l 'unsafe' crates/slipfork-engine/src | wc -l)" -le 2
cargo clippy -p slipfork-engine --all-targets -- -D warnings

# 3. Interactive terminal contract (skips cleanly when no tty is available).
cargo test -p slipfork-engine --test critical_engine -- --nocapture interactive

# 4. Two worlds can bind the same port while each dial reaches its own namespace.
cargo test -p slipfork-engine --test critical_engine -- --nocapture same_port
make check
```

All commands exit `0`. Adversarial assertions required: a dial after `destroy_world` fails with `WorldGone`, `stop_service` twice is idempotent, logs of a stopped service remain readable, and two worlds binding the same port stay isolated.
