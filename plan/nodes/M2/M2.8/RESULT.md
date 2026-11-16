# RESULT M2.8 — `slipfork-edge` part 1: socket activation, `LocalHostRoute` dispatch, `WorldRegistry`

**Status:** complete. Every `# Acceptance` command exits `0`; `crates/slipfork-edge/**` is the only crate
touched and `Cargo.toml` is untouched. `make check` was green on this crate's code and is currently red
only inside `crates/slipfork-ospp` (`M2.14`'s in-flight refactor, untouched here) — see
"Concurrent `slipfork-ospp` refactor" for the split-by-target evidence.

## What landed (`crates/slipfork-edge/**` only)

| File | Change |
|---|---|
| `src/activation.rs` | `adopt_systemd_listener()` (real `listenfd`, `fd 3` ordering, immediate `set_nonblocking(true)`), `ListenerSet`, `ListenConfig`, `bind_listen()`, `resolve_listener_set()`, `ACTIVATION_FIRST_FD` |
| `src/registry.rs` | `WorldRegistry::{new, mint_origin, attach_world, release_world, route, dial}`, `IngressRoute`, `RoutedWorld`, longest-prefix resolution, per-attach ingress-table validation |
| `src/ingress.rs` | `HttpIngress::bind`/`serve`, `0600` `<state_dir>/http-ingress.sock`, `httparse` HTTP/1.1 with a 16 KiB head cap, TCP+Unix shared dispatch, thread-per-connection relay through `dial_world_tcp` |
| `src/lib.rs` | `EdgeError` (activation / unix-ingress / registry variants) + re-exports; crate docs state the target DI contract, so `std::env` claim and not-yet-existing ports are no longer present-tense |
| `tests/critical_edge.rs` (new, **protected**) | 15 tests: real fd-3 handover, fd-order roles, fail-closed matrix, registry, `http-ingress.sock` |
| `plan/nodes/M2/M2.8/{RESULT,FOLLOWUPS}.md` | This result plus the two acceptance-untested deviations |

Untouched: `Cargo.toml`, `crates/slipfork-engine/**` (`M2.7`'s world-init/`http-ingress.sock` contract
consumed as-is). `src/daemon.rs`, `src/tunnel.rs`, `src/unit.rs` are **empty modules** whose one-line docs
say which node lands them (`M2.9` / `M2.9`+`M2.22` / `M2.10`).

### Truth & dedupe pass

- `daemon.rs` / `tunnel.rs` / `unit.rs` no longer claim `DaemonControl`, `UnavailableTunnelRegistrar`,
  `WeavedRegistrar`, `EdgeError::TunnelUnavailable`, or the `UnitSpec` renderer as present; each is one
  "not implemented yet; lands in M2.x" line over an empty module.
- `activation.rs` no longer cites the not-yet-declared `EdgeError::NoForwardListener` (prose instead), and
  references the `.socket` order `M2.10` **will** render rather than an existing `UnitSpec`/`crate::unit`.
- `registry.rs` module docs now state the truth: the **caller** supplies the empty-prefix
  `internal_ports[0]` catch-all; `attach_world` does not synthesize it.
- `ingress.rs`: forward listener merged into the HTTP listener chain
  (`http_listeners.into_iter().chain(forward_listener)`), and the relay chunk size is the named
  `RELAY_CAP` (16 KiB) instead of a literal (`HEADER_CAP` stays the header ceiling).

## Activation contract (`ADR-0008` §5)

- **Role assignment is positional, never port-based.** `LISTEN_FDS == 1` → `http_listeners = [fd 3]`,
  `forward_listener = None`; `LISTEN_FDS == n >= 2` → `fd 3..n+1` are HTTP ingress and `fd n+2` is the
  planned static forward listener (`UnitSpec`'s emitted order). `LISTEN_FDS=0`, a non-numeric count, a
  descriptor that is not a TCP listener, and a shifted descriptor set all fail closed
  (`ActivationBroken`); `LISTEN_PID != std::process::id()` (including an unset `LISTEN_PID` while
  `LISTEN_FDS` is set) fails closed (`ActivationMismatch`). No self-bind is ever attempted while the
  activation variables are present.
- **`set_nonblocking(true)` immediately per adopted descriptor**, plus a hard check that the adopted
  descriptor number *is* `3 + index` (rejecting a shifted `LISTEN_FDS_FIRST_FD`-style handover instead of
  serving the wrong sockets).
- **Discovered fact (`LESSONS` candidate, outside this node's paths):** `listenfd`'s `get_fds()` **clears**
  `LISTEN_PID`/`LISTEN_FDS` while adopting, so the handover is inherently single-shot. `resolve_listener_set`
  therefore reads the two variables **once** and drives the whole mode decision from that read
  (`Activation::read`), so a broken handover can never fall back to `--listen` even when one was passed.
  Startup must call `resolve_listener_set` (or `adopt_systemd_listener`) exactly once — documented on both.
- `--listen <addr>` binds the HTTP listener **only**; a forward listener exists only with
  `--forward-port <port>` (bound on `127.0.0.1`). Neither activation nor `--listen` → `EdgeError::NoListener`.

## Dispatch and the `0600` ingress socket (`H-011` #4 Variant a)

- `HttpIngress::bind` replaces a stale socket file but refuses one a live daemon still answers on
  (`IngressSocketInUse`), binds `<state_dir>/http-ingress.sock`, and restricts it to `0600`. `AF_UNIX`
  path budget is checked with `slipfork_engine::world_init::{unix_socket_path_fits, UNIX_SOCKET_PATH_MAX}`
  (reused, not re-implemented).
- TCP and Unix streams share one `ClientStream` (`Read + Write` + timeouts) and therefore one dispatch
  path: bounded head read (10 s, `<= 16 KiB`), `httparse`, single `Host` required, verb allow-list
  (`405` + `Allow` otherwise), `LocalHostRoute::parse` over every attached world's `snow_domains`
  (`404` on unknown host/target), longest-prefix `IngressRoute` match (`404` on an unmatched path),
  then `WorldEngine::dial_world_tcp` (`502` on failure) and a byte-transparent bidirectional relay with
  `Connection: close` toward the backend and half-close propagation. Errors in bodies name the world id,
  `Host`, path, and port; daemon-internal error strings stay in `EdgeError`, not in HTTP bodies.
- **Blocking accept loops:** adopted descriptors stay non-blocking (contract + tested `WouldBlock`) and
  `HttpIngress::bind` explicitly normalizes its listeners back to blocking, so a synchronous
  thread-per-connection server sleeps in the kernel instead of polling. Documented in-module.
- `serve()` monitors every accept loop through one `std::sync::mpsc` channel, so a failure on *any*
  listener (not only the first joined) is reported instead of being masked.

## `WorldRegistry`

- `mint_origin` wraps `WorldOrigin::local` (`:80` omitted, `:<port>` otherwise; `domains[].id` routes fail
  closed). `attach_world` rejects a duplicate active `(project, target)` (`WorldAlreadyRegistered`) and
  validates the table (`InvalidIngressRoute`): non-empty, `port != 0`, prefixes empty/`/`-rooted without
  whitespace/`?`/`#`, no duplicate prefix. `release_world` removes the entry **before** calling
  `destroy_world`, so a failed destroy never leaves the registry routing to a half-dead world; it returns
  `false` when nothing was registered.
- `route`/`dial` follow `ARCHITECTURE.md` §3.4: resolution borrows the lock only, dialing happens outside
  it. The empty prefix is the documented `internal_ports[0]` catch-all (shortest prefix, matches all);
  matching is path-segment aware (`/api` matches `/api`, `/api/x`, `/api?x` — never `/apifoo`).
- **Staged constructor:** `WorldRegistry::new(engine)` only. `Arc<dyn TunnelRegistrar>`,
  `http_ingress_port`, and `planned_forward_port` are `share_world` inputs (`M2.9`) and were deliberately
  not stored, because unreferenced private state would either trip `dead_code` or force contrived readers.
  `M2.9` adds them together with `share_world`/`unshare_world` and `EdgeError::NoForwardListener`.

## Tests (all real, no mocks of the contract)

- **Real fd-3 handover, in-process to a re-exec'd child:** the parent binds a listener, hands it to
  `sh` on `fd 0`, and the wrapper does `exec 3<&0 … LISTEN_PID=$$ LISTEN_FDS=<n> … exec <test binary>
  --exact activation_probe_child`. The child adopts, asserts the first `accept()` is `WouldBlock`,
  serves a real `404` through `HttpIngress` on the adopted fd, and the parent receives it — proving the
  descriptors were genuinely given away (the parent no longer owns them). `LISTEN_FDS=3` (three dup'd
  descriptors) proves the leading/trailing role split.
- The registry is exercised over the **real `IncusEngine`** adapter against an in-tree `/1.0` Unix-socket
  harness (the same pattern `slipfork-engine`'s critical suite uses, live `incusd` gated on `H-004`), so
  handles are minted by a real engine and `release_world` proves the destroy round-trip (harness counts
  the `DELETE`).
- `systemd-socket-activate` end-to-end is **not** part of `M2.8`'s body; it is `M2.10`'s protected test.

## Exact command outputs

```console
$ cargo test -p slipfork-edge --all-targets
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 15 tests
test activation_probe_child ... ok
test activation_descriptor_order_splits_forward_listener ... ok
test activation_listen_mode_without_forward_port ... ok
test activation_single_listener_handover ... ok
test fail_closed_no_listener_at_all ... ok
test fail_closed_non_socket_descriptor ... ok
test fail_closed_pid_mismatch_never_falls_back ... ok
test fail_closed_zero_descriptors ... ok
test registry_attach_rejects_duplicates_and_resolves_prefixes ... ok
test registry_mints_one_origin_per_world ... ok
test registry_rejects_invalid_ingress_tables ... ok
test routing_skeleton_rejects_unknown_methods_and_oversized_heads ... ok
test unix_ingress_sock_is_0600_and_serves_identical_404 ... ok
test unix_ingress_sock_refuses_live_socket ... ok
test unix_ingress_sock_replaces_stale_file ... ok
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture activation
running 4 tests   … test result: ok. 4 passed; 0 failed; 11 filtered out

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture fail_closed
running 4 tests   … test result: ok. 4 passed; 0 failed; 11 filtered out

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture registry
running 3 tests   … test result: ok. 3 passed; 0 failed; 12 filtered out

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture unix_ingress_sock
running 3 tests   … test result: ok. 3 passed; 0 failed; 12 filtered out

$ ! rg -q 'std::env' crates/slipfork-edge/src --glob '!activation.rs'     # exit 0
$ rg -n 'std::env' crates/slipfork-edge/src        # activation.rs:253 only
$ cargo clippy -p slipfork-edge --all-targets -- -D warnings              # exit 0
    Finished `dev` profile …           (only the pre-existing `unused dependency socket2` manifest note,
                                        declared by M2.2; Cargo.toml is out of this node's scope)
$ cargo fmt --all -- --check                                             # exit 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-edge --no-deps         # exit 0

# `make check` split by target (see "Concurrent ospp refactor" below):
$ make plan-check   # exit 0
$ make guard        # exit 0
$ make py-lint      # exit 0
$ make py-test      # exit 0
$ make ts-test      # exit 0
$ make rs-fmt       # exit 0
$ make docs-check   # exit 0  (zensical --strict OK, check_docs_honesty OK: 8 docs)
$ make rs-lint      # exit 2  FAILS in crates/slipfork-ospp (M2.14), not this crate
$ make rs-test      # exit 2  FAILS in crates/slipfork-ospp (M2.14), not this crate

# Proof that every crate outside the in-flight ospp pair is clean:
$ cargo clippy -p slipfork-edge -p slipfork-engine -p slipfork-world -p slipfork-snow \
      -p slipfork-dag -p slipfork-vcs --all-targets -- -D warnings        # exit 0
$ cargo test   -p slipfork-edge -p slipfork-engine -p slipfork-world -p slipfork-snow \
      -p slipfork-dag -p slipfork-vcs --all-targets                       # exit 0
  (edge 15, engine 14 live CoW, world 15, snow 12, dag 7 + 1, vcs 23 + 7 — all ok)

$ uv run --no-project tools/critical_guard.py --base 02836f1 --node M2.8
critical-guard: 02836f1..working tree, 50 changed file(s)
  M  crates/slipfork-ospp/tests/critical_ospp.rs  [VIOLATION: not covered by any node path]
  A  crates/slipfork-edge/tests/critical_edge.rs  [added (always allowed)]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M2.8.   (exit 1)
$ uv run --no-project tools/critical_guard.py --base 02836f1 --node M2.14
  M  crates/slipfork-ospp/tests/critical_ospp.rs  [authorized by crates/slipfork-ospp/**]
  A  crates/slipfork-edge/tests/critical_edge.rs  [added (always allowed)]
critical-guard: OK: authorized by M2.14                                     (exit 0)
```

### Concurrent `slipfork-ospp` refactor (`M2.14`) — why `make check` is red

An earlier `make check` on this crate's code returned **exit 0** (all targets). While this truth/dedupe
pass ran, `M2.14` began an in-flight uncommitted refactor of `crates/slipfork-ospp`
(`VerifiedDump`/`DiskVerifiedDump` trait work), so the workspace-wide `rs-lint`/`rs-test` now abort inside
that crate:

```console
$ make rs-lint
error[E0369]: binary operation `==` cannot be applied to type `Box<dyn slipfork_ospp::VerifiedDump>`
error[E0277]: `dyn slipfork_ospp::VerifiedDump` doesn't implement `std::fmt::Debug` (×6)
error: could not compile `slipfork-ospp` (test "critical_ospp") due to 9 previous errors
make: *** [Makefile:33: rs-lint] Error 101
$ make rs-test
error[E0432]: unresolved imports `dump::DUMP_SUFFIX`, `dump::DiskSeedDumpStore`, `dump::PART_SUFFIX`
error: could not compile `slipfork-ospp` (lib) due to 1 previous error
make: *** [Makefile:38: rs-test] Error 101
```

`slipfork-ospp` and `crates/slipfork` (which depends on it) were not touched by this node, and the scoped
`clippy`/`test` runs above plus `docs-check` prove the rest of the workspace is green. `M2.14`'s second
error was captured seconds later than the first, confirming the files were changing between runs.

**Guard note:** M2.8's only protected-test change is the new `tests/critical_edge.rs` (an allowed
addition, accepted in both runs). The single violation is `crates/slipfork-ospp/tests/critical_ospp.rs`,
which is `M2.14`'s concurrent in-flight edit in this shared checkout — authorized by `M2.14`'s
`crates/slipfork-ospp/**` paths and outside `M2.8`'s scope, so it was not touched.

## Follow-ups (for `M2.9` / `M2.10`)

The two acceptance-untested deviations are recorded in `plan/nodes/M2/M2.8/FOLLOWUPS.md`
(registry constructor params deferred to `M2.9`; adopted sockets flip back to blocking before serving).

1. `M2.9`: add `Arc<dyn TunnelRegistrar>`, `http_ingress_port`, `planned_forward_port` to
   `WorldRegistry::new` together with `share_world`/`unshare_world` and `EdgeError::NoForwardListener`;
   add `shared_hosts`, `308`/`421`, and `X-Forwarded-*` to the existing `dispatch`/`write_upstream_request`
   seam. `HttpIngress::serve` already serves the forward listener through the same dispatch.
2. `M2.9`: WebSocket upgrades on *local* worlds already pass through the byte-transparent relay because
   only hop-by-hop `Connection`/`Keep-Alive` are rewritten; shared-host `101` piping still needs the
   explicit rule set.
3. `M2.16`: call `resolve_listener_set(Some(&ListenConfig { addr, forward_port }))` exactly once at
   startup and pass `ListenerSet::http_port()` into the registry/`mint_origin` wiring.
4. `M2.10`: the unit renderer can now rely on the activation fd order implemented here; `socket2` remains
   declared but unused by this crate (pre-existing manifest entry from `M2.2`).
