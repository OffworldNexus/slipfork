# Result M2.9

Status: **done**

`crates/slipfork-edge` now owns the whole shared-world edge: `WorldRegistry` mints and revokes
tunnel leases, indexes public hostnames, decides every request (`proxy` vs `308` vs `421`), proxies
`shared_hosts` and local `*.localhost` traffic through `dial_world_tcp` with `Host` and
`X-Forwarded-*` intact, and exposes the complete typed `DaemonControl` port — including the
daemon-owned `forward` bridge — over `<runtime_dir>/daemon.sock` (`0600`, 64 KiB JSON-line frames).

## What landed

| File | Change |
|---|---|
| `src/registry.rs` | `WorldRegistry::new(engine, registrar, http_ingress_port, planned_forward_port)` (every field is read). `shared_hosts: BTreeMap<String,(ProjectSlug,WorldTarget)>` populated by `share_world`, cleared by `unshare_world`/`release_world`/lease drop, exact case-insensitive lookup (root dot and numeric port normalized). `shares` keeps each `WorldOrigin::Shared` plus the RAII `Box<dyn TunnelLease>`. `share_world` fails closed with `NoForwardListener` without a planned forward port, registers `TunnelRequest { route, local_port }` against it (never a `*.localhost` route), fails closed with `TunnelUnavailable`, and rejects a `*.localhost` lease URL (it would redirect-loop, `H-008`). `unshare_world` drops the lease and returns `WorldOrigin::Local`; idempotent with no lease. `release_world` clears the share and destroys the handle. `resolve(host, path, upgrade)` returns `RouteDecision::{Forward, Redirect, Misdirected}`; `origin()`, `dial`, `dial_port`, `forward`. |
| `src/ingress.rs` | The routing decision runs before the method allowlist, so a shared world's `*.localhost` host answers `308` for **every** method (`Location: <shared><path_and_query>`) before any `100 Continue`, and `421` for `Connection: Upgrade`; neither path dials a backend. `HEAD` gets the status + `Location` but no body and no `Content-Length` (RFC 9110 §9.3.2), both status responses sharing one `write_status` renderer. Proxied requests preserve `Host`, restate `X-Forwarded-Host`/`X-Forwarded-Proto`, append this hop to `X-Forwarded-For`, and keep `Connection: Upgrade` + `Upgrade` so WebSocket handshakes proxy through as a raw byte pipe. The response head is sniffed first: a `101` clears the idle bound on both sockets (an upgraded WebSocket is never torn down after 60 s of silence); `HttpIngress::with_relay_timeout` injects a short bound in tests. Uses the shared `0600` binder and the shared relay. |
| `src/unix_socket.rs` (new) | One canonical `0600` pathname-socket binder (`http-ingress.sock` + `daemon.sock`): `sun_path` budget check, parent creation, stale-file replacement, live-socket refusal, `set_permissions(0600)`. Deleted the duplicated ingress-only helpers and their error variants. |
| `src/forward.rs` (new) | `ForwardTargetSpec { id, port, scheme, path }` (validated) and `ForwardBridge`: binds `127.0.0.1:0` per target in-process, one non-blocking accept loop with a stop flag, each connection piped to `WorldEngine::dial_world_tcp` with half-close; `Drop` stops the loops and joins, closing every host port. |
| `src/daemon.rs` + `src/daemon/{server,client,wire}.rs` (new) | The complete 17-method `DaemonControl` trait (`status`, `up`, `down`, `fork`, `drop_fork`, `list`, `prune`, `share`, `unshare`, `secret_set/unset/list`, `logs`, `exec_target`, `hook_run`, `forward`, `auth_login`) with typed wire types. Every cwd-sensitive request carries `cwd: PathBuf`; `exec_target`'s `WorldExecTarget` is the only reply carrying `ExecEnv`; `ControlSecret` redacts in `Debug`. Server binds `0600` `daemon.sock`, 10 s request-head bound, 64 KiB frame cap with a buffered `FrameReader`, 8 KiB stream-chunk splitting. `DaemonControlClient` implements the same trait, one connection per call; `forward` publishes the summary and holds the connection for the bridge's lifetime. |
| `src/tunnel.rs` | `UnavailableTunnelRegistrar`: the fail-closed `TunnelRegistrar` wired until `weaved`'s IPC spec exists (`H-006`/`M2.22`); deleted on `M2.22`. |
| `src/relay.rs` (new) | The bounded relay: `copy` plus one `pipe(duplex_a, duplex_b)` that owns the stream clones, the uplink thread, half-close propagation, the double shutdown, and the join. Both the ingress relay and `forward.rs` call it; no second clone/uplink dance remains. |
| `src/lib.rs` | New exports; `EdgeError` gained `SocketPath/SocketInUse/SocketBind/SocketDir` (replacing the ingress-only variants), `WorldRoute`, `NoForwardListener`, `TunnelUnavailable`, `SharedHostConflict`, `ForwardSpec`, `ControlIo`, `ControlFrame`, `ControlProtocol`, `Control(DaemonError)`. |
| `tests/critical_edge.rs` | 15 existing tests updated to the 4-argument constructor / `resolve` / `SocketInUse`, plus 12 new tests: `InMemoryTunnelRegistrar` (records route + `local_port`, counts lease revocation), a `DialEngine` test double (real loopback TCP behind the engine port, in-tree Incus harness for clone/destroy), real HTTP/WebSocket backends, the acceptance filters below, and an idle-gap WebSocket test driven by an injected 200 ms relay bound (verified to fail when the `101` timeout clear is removed). |

## Acceptance (all commands exit 0)

### 1. Shared-host proxying preserves `Host` and `X-Forwarded-*`

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture shared_proxy
running 2 tests
test shared_proxy_preserves_host_and_forwarded_headers ... ok
test shared_proxy_streams_body_larger_than_buffer ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 24 filtered out; finished in 0.00s
```

The backend echoes the received head: `Host: acme-abc.weaved.example` verbatim,
`X-Forwarded-Host` = public host, `X-Forwarded-For: 203.0.113.9, 127.0.0.1` (front hop preserved,
this hop appended), `X-Forwarded-Proto: http`, request target and query unchanged, backend dialed
exactly once. The streaming test sends only the head of a 2 MiB body first: the backend answers
before any body exists (proving the head is forwarded, not buffered), the body then streams
through the 16 KiB relay and arrives complete.

### 2. `308` (including `Expect: 100-continue`) and `421` on `Upgrade`

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture redirect
running 1 test
test redirect_308_for_every_method_and_before_100_continue ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out; finished in 0.00s

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture upgrade_refused
running 1 test
test upgrade_refused_421_on_shared_localhost ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out; finished in 0.00s
```

`GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS/BREW` on `acme.localhost`,
`admin.acme.localhost` (declared `snow.yml` domain) and `acme.localhost:8080` all answer `308` with
`Location: https://acme-abc.weaved.example/deep/path?x=1`, and the backend connection counter stays
`0`. A `POST` with `Expect: 100-continue` and a 1 MiB `Content-Length` but no body receives the
`308` with **no** `100 Continue`. `Connection: Upgrade` + `Upgrade: websocket` on `acme.localhost`
answers `421 Misdirected Request` naming the shared URL, with zero backend connections.

### 3. WebSocket byte piping over a shared host

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture websocket_pipe
running 2 tests
test websocket_pipe_survives_fragmentation ... ok
test websocket_pipe_survives_an_idle_gap_longer_than_the_relay_timeout ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out; finished in 0.50s
```

Handshake over the **public** shared host proxies to `101 Switching Protocols`; 4 KiB of payload
written in 63-byte fragments is echoed byte-for-byte (the backend echoes in 7-byte reads/writes), so
neither side re-frames the post-101 stream. The idle-gap test arms a 200 ms relay bound, waits
500 ms of complete silence after the `101`, then sends and reads an echo: an upgraded socket is
never subject to the idle bound (removing the clear makes this test fail).

### 4. Control IPC, daemon-owned `forward` lifetime, share/unshare asymmetry, gates

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture daemon_control
running 3 tests
test daemon_control_preserves_typed_errors ... ok
test daemon_control_round_trip_and_frame_cap ... ok
test daemon_control_forward_lifetime_closes_host_listener ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out; finished in 0.05s

$ cargo test -p slipfork-edge --all-targets
running 0 tests
test result: ok. 0 passed; 0 failed
running 27 tests
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.56s

$ cargo clippy -p slipfork-edge --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.65s

$ cargo fmt -p slipfork-edge -- --check
FMT_EXIT=0

$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-edge --no-deps
Generated .../target/doc/slipfork_edge/index.html
DOC_EXIT=0

$ make check
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
MAKE_CHECK_EXIT=0
```

Round trip over `daemon.sock` proves: `0600` mode; typed `cwd` crosses the wire (the harness
records it) for `up`/`down`/`fork`/`drop_fork`/`share`/`unshare`/`exec_target`/`forward`; both boot
events stream before the summary; a 20 KiB log chunk is split into multiple frames under the 64 KiB
cap and reassembles; `ExecEnv` `Debug` redacts secrets; share mints `lease-1` and
`https://acme-abc.weaved.example`, unshare returns `http://acme.localhost` twice (idempotent);
an oversized request frame is refused with `{"kind":"frame_too_large"}`; a second server with an
`UnavailableTunnelRegistrar`/no forward port surfaces `DaemonErrorKind::NoForwardListener` across
the socket.

`daemon_control_forward_lifetime_closes_host_listener` connects through the daemon-bound
`127.0.0.1:<ephemeral>` to the echo backend (`ping` -> `ping`, URI `tcp://<addr>`), returns from the
callback (client disconnect), and polls until the host listener refuses connections.

`share_world_fails_closed_without_forward_listener_or_registrar` proves `NoForwardListener` and
`TunnelUnavailable`, then a `http://acme.localhost` lease URL is refused (redirect-loop guard).
`unshare_world_is_idempotent_on_local_world` proves two no-lease unshares revoke nothing, a share
redirects with the path+query, and unshare revokes the lease (counter 1), restores the port-qualified
local origin, and stops proxying the public host. `release_world_clears_the_share_and_revokes_the_lease`
proves release drops the lease and the public host 404s afterwards.

### Protected-test guard

```text
$ uv run --no-project tools/critical_guard.py --base cbccafb --node M2.9
critical-guard: cbccafb..working tree, 51 changed file(s)
  M  crates/slipfork-edge/tests/critical_edge.rs  [authorized by crates/slipfork-edge/**]
critical-guard: OK: authorized by M2.9

$ uv run --no-project tools/guard.py
guard: OK (293 file(s) checked)
```

## Quad-gate fixes (post-review)

- **Upgraded WebSockets are no longer torn down by the idle bound.** `relay` now reads the backend
  response head first; on `101 Switching Protocols` it clears the read/write timeout on both
  sockets before piping, so a silently idle WebSocket lives as long as its peers. Every other
  exchange keeps the 60 s idle bound, and the request-head bound (`HEAD_READ_TIMEOUT`, 10 s) is unchanged.
  `HttpIngress::with_relay_timeout` lets the critical suite inject 200 ms and prove the behaviour.
- **`HEAD` responses carry no body and no `Content-Length`.** `308` and `421` share one
  `write_status(client, status, reason, location, body, head_only)` renderer; `HEAD` emits the
  status line and `Location` only, as RFC 9110 §9.3.2 requires. The redirect test asserts an empty
  body after the head terminator.
- **One bidirectional relay.** `relay::pipe(&mut a, &mut b)` owns the clones, the uplink thread,
  half-close propagation, the double shutdown, and the join (via a crate-private `Duplex` trait
  implemented for `TcpStream` and the ingress `ClientStream`). `forward.rs`'s `bridge` is now three
  lines and the two copies can no longer drift.
- **One status-head writer** (above), and the crate doc no longer claims `socket2` is used.

## Deviations / deferrals

- **`WorldRegistry::new` gained all four collaborators now** (`engine`, `registrar`,
  `http_ingress_port`, `planned_forward_port`), closing the `M2.8` deviation recorded in
  `plan/JOURNAL.md`; no constructor field is dead.
- **`shared_hosts` is backed by a second `shares` index.** The pinned value type
  `(ProjectSlug, WorldTarget)` cannot own a `Box<dyn TunnelLease>`, so the lease record
  (`origin` + RAII lease) lives in a parallel `Vec<ActiveShare>` mutated under the same lock; the two
  are kept in bijection by `share_world` / `unshare_world` / `release_world`, with a conflict check
  that fails closed (`SharedHostConflict`) before any mutation.
- **`X-Forwarded-For` is appended, not replaced.** The front hop's value is preserved and this
  hop's peer is appended (standard proxy chain); `X-Forwarded-Host`/`Proto` are restated per hop.
- **`DaemonControl` payloads beyond the ADR/ARCHITECTURE shapes are defined here**, because
  `DaemonControl` is Slipfork's own CLI-to-daemon protocol (unlike `weaved`'s gated IPC):
  `DaemonStatus`, `WorldSummary`, `WorldUpSummary`/`ResourceSummary`, `BootEvent`,
  `PruneReport`, `AuthSummary`, `ForwardTarget`/`ForwardSummary`, `DumpRequest`/`ResourceRequest`,
  and the typed `DaemonError`/`DaemonErrorKind`. They follow `docs/design/target-dx.md` §1.5.
- **Forward connections carry no idle timeout.** `forward` is a raw TCP bridge (`ssh -L`
  semantics); the listener set is bound to the control connection and torn down on disconnect.
  In-flight bridged connections end when either peer closes.
- **`socket2` remains declared but unused** in `crates/slipfork-edge/Cargo.toml` (pre-existing since
  `M2.8`); removing it also touches the root `[workspace.dependencies]`, outside this node's paths.
- No production tunnel adapter was invented: `share` fails closed with
  `UnavailableTunnelRegistrar`, and `M2.22` deletes that adapter when `weaved` publishes its IPC.

## Follow-ups

- `M2.20`/`M2.10` audit: drop the unused `socket2` dependency from both `crates/slipfork-edge` and
  the workspace table (single sweep, no dual state).
- `M2.10`: render `slipforkd.socket`/`.service` over `UnitSpec`; the activation order it emits must
  keep matching `M2.8`'s fd-order roles.
- `M2.16`: implement `DaemonControl` in the `slipforkd` Composition Root (wire `FileVaultProvider`,
  `ReflinkNetnsEngine`, `ResourcePlan`, `VcsWorkspaceFactory`, `UnavailableTunnelRegistrar`,
  `DaemonControlServer`, `HttpIngress`) and drive it from the thin CLI via `DaemonControlClient`
  (auto-spawn on missing `daemon.sock`); resolve `forward` target ids to
  `ForwardTargetSpec { port, scheme, path }` there.
- `M2.22`: replace `UnavailableTunnelRegistrar` with the concrete `WeavedRegistrar` and prove
  `share_unshare` against the real adapter.
