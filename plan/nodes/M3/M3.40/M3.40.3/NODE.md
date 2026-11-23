---
id: M3.40.3
title: "Stateless :80 .localhost HTTP + WebSocket ingress router and streaming proxy"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.40.2]
paths: ["crates/slipfork-ingress/**", Cargo.toml, Cargo.lock, "specs/03-under-the-hood.md", TESTING_PLAN.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T13:18"
---

# Goal
Replace the `IngressRouter` stub with a real stateless `.localhost` HTTP + WebSocket ingress router/proxy over an injected listener.

# Context
- `crates/slipfork-ingress/src/lib.rs` is a 12-line `IngressRouter` marker stub; `Cargo.toml` has an empty `[dependencies]`.
- Spec: `specs/03-under-the-hood.md` §4 (route matching modes, `LISTEN_FDS` handoff, one accepted connection fd forwarded to the daemon) and the URL shapes in `specs/02-cli-ux.md`.
- Authorities: `<world>.localhost`, `<domain>.<world>.localhost`, `<prefix>.*`, `<cdn>.<world>.localhost`, `<fork>--<world>.localhost`; modes keep-prefix (default), strip-prefix, rewrite, exact.
- Accept an injected `TcpListener`/accepted `UnixStream` only: zero `std::env`, zero `/proc` probes, zero socket binding in library code.
- Stream request/response bodies (bounded buffering - never `read_to_end` a body before forwarding, which deadlocks uploads) and pump WebSocket bytes transparently after the upgrade handshake.
- This crate knows nothing about worlds or daemons: the caller supplies the accepted fd and the route table.

# Acceptance
- `cargo test -p slipfork-ingress`: real loopback TCP + `socketpair` E2E covers every authority/mode mapping to the expected upstream path, a 10 MiB streaming upload with bounded memory, and bidirectional WebSocket frames.
- `cargo clippy -p slipfork-ingress --all-targets -- -D warnings`.
- `! grep -rn 'std::env\|/proc/' crates/slipfork-ingress/src`.
- `uv run --no-project tools/critical_guard.py`.
- `specs/03` §4 matches the shipped matcher (update only where implementation pinned a detail).
