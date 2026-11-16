# FOLLOWUPS M2.8

## 1. `WorldRegistry::new` takes only `Arc<dyn WorldEngine>` (deferred to M2.9)

`NODE.md` #4 describes the constructor as `Arc<dyn WorldEngine>` + `Arc<dyn TunnelRegistrar>` +
`http_ingress_port: u16` + `planned_forward_port: Option<u16>`. `M2.8` stores only the engine because the
other three are `share_world`/`unshare_world` inputs that do not exist yet: unreferenced private state
would either trip `dead_code` (a hard gate) or force contrived readers. `M2.9` must add all three fields
together with `share_world`/`unshare_world` and `EdgeError::NoForwardListener` (which also has no producer
today, so it was not declared).

## 2. Adopted sockets flip back to blocking mode before serving (deliberate, untested by acceptance)

`adopt_systemd_listener` leaves every inherited descriptor **non-blocking** (`ADR-0008` §5, asserted with
a real `WouldBlock` in `critical_edge.rs`), then `HttpIngress::bind` normalizes its TCP listeners and the
`AF_UNIX` ingress socket back to **blocking** so the synchronous thread-per-connection accept loops sleep
in the kernel instead of polling. The acceptance suite only pins the adopted (non-blocking) state, never
the normalization. `M2.10`/`M2.16` must keep the invariant ordering: adopt → assert/observe
non-blocking → `HttpIngress::bind` (the only blocking-mode boundary), and must not add a second accept loop
that assumes non-blocking descriptors.
