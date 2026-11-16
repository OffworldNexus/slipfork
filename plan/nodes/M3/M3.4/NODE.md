---
id: M3.4
title: "Isolated: concrete WeavedRegistrar IPC adapter and declared support promotion (gated on H-006)"
kind: task
status: blocked
owner: implementer
priority: 3
depends_on: [M3.1]
paths:
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
  - "crates/slipfork/src/**"
  - support/integrations.toml
  - "docs/support-matrix.md"
  - "docs/reference/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T06:34"
---

# Goal

Implement the concrete `WeavedRegistrar` tunnel-registrar wire adapter inside `crates/slipfork-edge` (deleting `UnavailableTunnelRegistrar`) and promote the tunnel integration in `support/integrations.toml` — **only** once `tunnel-weaver` publishes `weaved`'s IPC wire specification (`H-006`). Carried forward from `M2.22` post-`0.2.0`; while absent, perform zero edits and stay `blocked`.

# Context

- `crates/slipfork-edge` ships `WorldRegistry::share_world` / `unshare_world`, `shared_hosts` proxying, `308`/`421` `*.localhost` rules, and `<= 31`-byte `WeaverServiceName` truncation (`ADR-0005` §2, `ADR-0008` §6), wired to fail-closed `UnavailableTunnelRegistrar` per deferred `H-006`.
- Terminal and isolated: nothing in `M3` depends on this node.

# Design / Invariants

1. Check for a published `weaved` IPC specification (`plan/HUMAN.md` / `docs/research/*weaved*ipc*`); if absent, perform zero edits and stay `blocked` (`missing-input`, `H-006`).
2. When published: implement `WeavedRegistrar: TunnelRegistrar` (injected endpoint/credentials, zero `std::env` outside `slipforkd`), delete `UnavailableTunnelRegistrar`, pass `TunnelRegistrar` + `share_world`/`unshare_world` critical tests, and promote tunnel sharing to `declared` via `make support-sync`.

# Acceptance

```bash
uv run --no-project python -c 'import pathlib; hits=[p.as_posix() for p in pathlib.Path("docs/research").glob("*weaved*ipc*")]; print("H-006: wire spec present at " + hits[0]) if hits else print("H-006: absent -> zero edits, STATUS: blocked (missing-input)")'
```
