---
id: M2.9
title: "slipfork-edge: shared_hosts proxy, 308/421 rules, share/unshare, DaemonControl IPC"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.8]
paths: ["crates/slipfork-edge/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

`crates/slipfork-edge` part 2: proxy shared hostnames from the registry's `shared_hosts` map into the world through `WorldEngine::dial_world_tcp` (preserving `Host` and `X-Forwarded-*`, HTTP and WebSocket alike), enforce the `*.localhost` `308`/`421` rules for shared worlds, and expose `share_world`/`unshare_world` plus the daemon control IPC that the CLI drives.

# Context

- `H-008` (closed): `weaved` forwards the public hostname to `slipforkd`'s planned static port, never to `*.localhost:80` (that would redirect-loop) and never into a world namespace directly. Therefore shared hostnames are **proxied, never redirected**.
- `ADR-0005` section 1: for a world whose origin is shared, every non-upgrade request on any `*.localhost` host resolving to it must answer `308 Permanent Redirect` for every method, and the `308` must be sent **before** `100 Continue` when the client sends `Expect: 100-continue`. Every `Upgrade`/WebSocket request on such a host must answer `421 Misdirected Request` with a diagnostic naming the shared URL.
- `DaemonControl` is Slipfork's own CLI-to-daemon control port, not a tunnel protocol; the concrete `TunnelRegistrar` wire adapter stays gated on `H-006`.

# Design / Invariants

1. **`shared_hosts`:** `BTreeMap<String, (ProjectSlug, WorldTarget)>` owned by `WorldRegistry`, populated by `share_world` and cleared by `unshare_world`, `release_world`, or lease drop. Lookup is exact-host and case-insensitive; a miss falls through to `*.localhost` parsing.
2. **Proxy path:** open one `TcpStream` via `dial_world_tcp(&world, port)` per request, forward the request line and headers with `Host` preserved, plus `X-Forwarded-Host`, `X-Forwarded-For`, `X-Forwarded-Proto`; stream the body in both directions chunk-by-chunk with a bounded buffer (no full-body buffering) and half-close propagation.
3. **WebSocket/`Upgrade` on a shared host:** after forwarding the `101` response, the connection becomes a raw bidirectional byte pipe until either side closes; no frame parsing, no re-encoding.
4. **`*.localhost` rules for shared worlds:** `308` with `Location: <shared_url><path_and_query>` for every method (method and body preserved per RFC 9110 section 15.4.9; when `Expect: 100-continue` is present, respond `308` immediately without waiting for or reading the body); `421` for `Connection: Upgrade` requests. Local worlds are proxied normally.
5. **`share_world`/`unshare_world`:** `share_world` mints the shared origin from `TunnelRegistrar::register(&TunnelRequest { route, local_port: planned_port })` **before** any materialization and returns `SharedWorldInfo { public_url, lease_id }`; `unshare_world` drops the lease (RAII revoke) and returns the restored `WorldOrigin::Local`. When no registrar is wired, both fail closed with `EdgeError::TunnelUnavailable` (a diagnostic naming `H-006`) — never a fake local tunnel.
6. **`DaemonControl` IPC:** JSON-line request/response over a Unix socket at `$XDG_RUNTIME_DIR/slipfork/slipforkd.sock` (path injected at the composition root, `0600`), one request per line, bounded frame size, and an error response that never leaks secret values.

# Acceptance

```bash
# 1. Shared-host proxying preserves Host and X-Forwarded-* against a real backend.
cargo test -p slipfork-edge --test critical_edge -- --nocapture shared_proxy

# 2. 308 (including Expect: 100-continue ordering) and 421 on Upgrade.
cargo test -p slipfork-edge --test critical_edge -- --nocapture redirect
cargo test -p slipfork-edge --test critical_edge -- --nocapture upgrade_refused

# 3. WebSocket byte piping over a shared host survives fragmentation.
cargo test -p slipfork-edge --test critical_edge -- --nocapture websocket_pipe

# 4. Control IPC round-trip and fail-closed tunnel path.
cargo test -p slipfork-edge --test critical_edge -- --nocapture daemon_control
cargo test -p slipfork-edge --all-targets
cargo clippy -p slipfork-edge --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a shared world's `*.localhost` request is never proxied (always `308`), an `Upgrade` on that host is never proxied (always `421`), `share_world` without a registrar returns `TunnelUnavailable`, and a proxy body larger than the buffer streams without buffering the whole request in memory.
