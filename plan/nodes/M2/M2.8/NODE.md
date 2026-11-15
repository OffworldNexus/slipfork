---
id: M2.8
title: "slipfork-edge: systemd socket activation, LocalHostRoute dispatch, WorldRegistry"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.3, M2.7]
paths: ["crates/slipfork-edge/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Stand up `crates/slipfork-edge` part 1: unprivileged systemd socket activation for `slipforkd` (`LISTEN_PID`/`LISTEN_FDS`, `fd >= 3`, immediate non-blocking adoption, explicit `--listen` for tests, fail-closed otherwise), the request dispatch skeleton over `LocalHostRoute`, and the daemon-owned `WorldRegistry` with `register_world`/`release_world` returning the world's single `WorldOrigin`.

# Context

- `H-008` (closed) makes `slipforkd` the sole ingress for both `Local` and `Shared` worlds, and `ADR-0005` section 1 fixes local routing to `http://(<snow-domain>.)?(<fork>.)?<project>.localhost` with a `404` for unknown targets.
- Measured host facts (`docs/research/tunnel-weaver-and-systemd-facts.md` section 5): activation sockets start at `fd 3`, `LISTEN_PID` equals the child pid, and inherited sockets are **blocking** — the daemon must set non-blocking itself. Binding port 80 unprivileged fails on this host (`ip_unprivileged_port_start = 1024`), which is exactly why `--listen` exists.
- The crate skeleton, `slipfork-world`/`slipfork-engine` dependencies, and the workspace socket-activation helper are already declared by `M2.2`; this node must not touch `Cargo.toml`.

# Design / Invariants

1. **Activation module:** `adopt_systemd_listener() -> Result<Option<Vec<TcpListener>>, EdgeError>` reads the activation environment exactly once at the composition boundary, requires `LISTEN_PID == getpid()`, reads `LISTEN_FDS` sockets from `fd 3..3+N`, and calls `set_nonblocking(true)` **before** adopting each socket. Mismatched `LISTEN_PID` or a failed adoption is a hard error (`EdgeError::ActivationMismatch` / `EdgeError::ActivationBroken`), never a silent self-bind.
2. **Listener selection:** exactly one source of truth — activation sockets when present, otherwise `--listen <addr>` (required for unprivileged runs), otherwise `EdgeError::NoListener`. Zero fallbacks between the two.
3. **Dispatch skeleton:** parse the `Host` header with `slipfork_world::LocalHostRoute::parse(&project, &snow_domains, host)`; unknown host or unknown target answers `404` with an actionable body. Per-project routing data is injected through the registry (no ambient cwd/env reads, no `std::env` in this crate outside the activation boundary).
4. **`WorldRegistry`:** constructed as `WorldRegistry::new(engine: Arc<dyn WorldEngine>, registrar: Arc<dyn TunnelRegistrar>, planned_port: u16)`; `register_world(&ProjectSlug, &WorldTarget) -> Result<WorldOrigin, EdgeError>` mints `WorldOrigin::Local` via `LocalHostRoute::url()` and stores the handle; `release_world` destroys the world through the engine and returns `true`/`false`. Concurrent registration of the same `(project, target)` fails closed (`EdgeError::WorldAlreadyRegistered`).
5. **Server loop:** `std::thread` per accepted connection is acceptable (no async runtime required); the read timeout is bounded so a slow client cannot pin a thread, and HTTP/1.1 request parsing is strict (reject non-`GET`/`POST`/`PUT`/`PATCH`/`DELETE`/`HEAD`/`OPTIONS` with `405`).
6. `#![forbid(unsafe_code)]`; zero `unwrap()` on I/O paths; every error carries the world id and the host header.

# Acceptance

```bash
# 1. Activation contract proven with a real fd-3 handover in-process.
cargo test -p slipfork-edge --all-targets
cargo test -p slipfork-edge --test critical_edge -- --nocapture activation

# 2. Fail-closed paths: mismatched LISTEN_PID, broken fd, no listener at all.
cargo test -p slipfork-edge --test critical_edge -- --nocapture fail_closed

# 3. Routing skeleton answers 404 for unknown hosts and registers one origin per world.
cargo test -p slipfork-edge --test critical_edge -- --nocapture registry

# 4. Gates.
! rg -q 'std::env' crates/slipfork-edge/src --glob '!activation.rs'
cargo clippy -p slipfork-edge --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: an activation environment whose `LISTEN_PID` differs from the child pid is refused, an adopted socket is non-blocking (first `accept` without a pending connection returns `WouldBlock`), and `--listen` plus activation sockets is resolved by the documented single precedence rule.
