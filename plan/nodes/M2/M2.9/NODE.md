---
id: M2.9
title: "slipfork-edge: shared_hosts proxy, 308/421 rules, share/unshare, DaemonControl IPC"
kind: task
status: review
owner: implementer
priority: 3
depends_on: [M2.8]
paths: ["crates/slipfork-edge/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T20:28"
---

# Goal

`crates/slipfork-edge` part 2: proxy shared hostnames from `WorldRegistry`'s `shared_hosts` map into the world through `WorldEngine::dial_world_tcp` (preserving `Host` and `X-Forwarded-*`, HTTP and WebSocket alike), enforce the `*.localhost` `308`/`421` rules for shared worlds, and expose `WorldRegistry::share_world`/`unshare_world` plus the `<runtime_dir>/daemon.sock` `DaemonControl` RPC (including daemon-owned `forward`) that the CLI drives.

# Context

- `H-008` (closed): `weaved` forwards the public hostname to `slipforkd`'s planned static `forward_port`, never to `*.localhost` (that would redirect-loop) and never into a world namespace directly. Therefore shared hostnames are **proxied, never redirected**.
- `ADR-0005` section 1: for a world whose origin is shared, every non-upgrade request on any `*.localhost` host resolving to it must answer `308 Permanent Redirect` for every method (`308` sent **before** `100 Continue` when `Expect: 100-continue` is present), and every `Upgrade`/WebSocket request on such a host must answer `421 Misdirected Request` naming the shared URL.
- `DaemonControl` is Slipfork's own CLI-to-daemon control port over `<runtime_dir>/daemon.sock` (`0600`), not a tunnel protocol; the concrete `TunnelRegistrar` wire adapter stays gated on closed-by-deferral `H-006`.

# Design / Invariants

1. **`shared_hosts`:** `BTreeMap<String, (ProjectSlug, WorldTarget)>` owned by `WorldRegistry`, populated by `WorldRegistry::share_world` and cleared by `WorldRegistry::unshare_world`, `WorldRegistry::release_world`, or lease drop. Lookup is exact-host and case-insensitive; a miss falls through to `*.localhost` parsing.
2. **Proxy path (`IngressRoute` longest-prefix -> `dial_world_tcp`):** Match the request path against the target world's `IngressRoute` longest-prefix table, open one `TcpStream` via `dial_world_tcp(&world, port)`, forward request line + headers with `Host` preserved plus `X-Forwarded-{Host,For,Proto}`, and stream bodies chunk-by-chunk with half-close propagation. Include `InMemoryTunnelRegistrar` in `tests/` to verify `shared_hosts` + `308`/`421` end-to-end.
3. **WebSocket/`Upgrade` on a shared host:** after forwarding the `101` response, the connection becomes a raw bidirectional byte pipe until either side closes; no frame parsing, no re-encoding.
4. **`*.localhost` rules for shared worlds:** `308` with `Location: <shared_url><path_and_query>` for every method (method and body preserved per RFC 9110 section 15.4.9; when `Expect: 100-continue` is present, respond `308` immediately without waiting for or reading the body); `421` for `Connection: Upgrade` requests. Local worlds are proxied normally.
5. **`WorldRegistry::share_world` / `unshare_world` asymmetry (`ADR-0005` §2):** `WorldRegistry::share_world` requires `planned_forward_port` to be `Some(local_port)` (else fails closed with `EdgeError::NoForwardListener`) and mints `WorldOrigin::Shared` from `TunnelRegistrar::register(&TunnelRequest { route, local_port })` (failing closed with `EdgeError::TunnelUnavailable` when `UnavailableTunnelRegistrar` is wired); `WorldRegistry::unshare_world` drops any active lease and returns `WorldOrigin::Local` — acting as an **idempotent success no-op** when the world is already `Local` / has no lease.
6. **Full `DaemonControl` `<runtime_dir>/daemon.sock` (`0600`) RPC & Daemon-Owned `forward` (`H-010` #1):** Define the complete typed `DaemonControl` trait + bounded (64 KiB frame cap) JSON-line Unix socket server and `DaemonControlClient` bound at explicit `<runtime_dir>/daemon.sock` (`0600`; never guessing `$XDG_RUNTIME_DIR`). Every `cwd`-sensitive request (`UpRequest`, `WorldSelector`, `ForkRequest`, `LogsRequest`, `HookRunRequest`, `ExecTargetRequest`, `ForwardRequest`) carries explicit `cwd: PathBuf` so the daemon never resolves its own cwd. `DaemonControl::{share, unshare}` map 1:1 onto `WorldRegistry::{share_world, unshare_world}`. `DaemonControl::exec_target` returns `WorldExecTarget` (the **only** RPC reply permitted to carry `ExecEnv`; errors never leak secrets). `DaemonControl::forward` makes `slipforkd` bind host `127.0.0.1:0` listeners **in-process** and bridge each accepted connection to `WorldEngine::dial_world_tcp` inside `slipforkd`, replying `{world, targets:[{id, host_addr, uri}]}` and keeping the bridge alive for the lifetime of that open control connection (client disconnect / Ctrl-C tears the host listeners down; zero FD handover, zero stream-upgrade, zero `SCM_RIGHTS`).

# Acceptance

```bash
# 1. Shared-host proxying preserves Host and X-Forwarded-* against a real backend.
cargo test -p slipfork-edge --test critical_edge -- --nocapture shared_proxy

# 2. 308 (including Expect: 100-continue ordering) and 421 on Upgrade.
cargo test -p slipfork-edge --test critical_edge -- --nocapture redirect
cargo test -p slipfork-edge --test critical_edge -- --nocapture upgrade_refused

# 3. WebSocket byte piping over a shared host survives fragmentation.
cargo test -p slipfork-edge --test critical_edge -- --nocapture websocket_pipe

# 4. Control IPC round-trip, daemon-owned forward lifetime, and share_world/unshare_world asymmetry.
cargo test -p slipfork-edge --test critical_edge -- --nocapture daemon_control
cargo test -p slipfork-edge --all-targets
cargo clippy -p slipfork-edge --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a shared world's `*.localhost` request is never proxied (always `308`), an `Upgrade` on that host is never proxied (always `421`), `share_world` without a forward listener returns `NoForwardListener` and without a registrar returns `TunnelUnavailable`, `unshare_world` on a local world succeeds idempotently, dropping a `forward` control connection closes the bound `127.0.0.1:<ephemeral>` listener, and a proxy body larger than the buffer streams without buffering the whole request in memory.
