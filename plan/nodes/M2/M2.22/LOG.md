# Log M2.22

## 2026-11-15T16:40 [planner]

created (task) Isolated: concrete WeavedRegistrar IPC adapter and declared support promotion (gated on H-006)

## 2026-11-16T00:26 [lead]

status ready -> blocked: H-006 closed by deferral: no weaved IPC wire specification exists, so WeavedRegistrar stays unimplemented and share stays fail-closed (EdgeError::TunnelUnavailable). Reopens when tunnel-weaver publishes its spec.

## 2026-11-16T06:34 [strategist]

status blocked -> superseded: Carried forward post-0.2.0 to M3.4 (still gated on deferred H-006 weaved IPC spec)
