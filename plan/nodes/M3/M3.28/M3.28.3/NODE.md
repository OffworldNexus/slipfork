---
id: M3.28.3
title: "DaemonControl truth: bound ingress addresses, resolved origin port, world state, and the per-world component tree"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.2]
paths: ["crates/slipfork-edge/src/**", "crates/slipfork-edge/tests/**", crates/slipfork/src/daemon.rs]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T17:53"
---

# Goal

Expose the daemon's own truth through the typed `DaemonControl` seam: bound ingress addresses, resolved origin port, world state, and a per-world component tree (pid, uptime, port), plus the `--json` schema that carries it.

# Context

The owner saw `[origin] http://demo.localhost:40645` and had no way to learn which address was bound or which port the edge resolved (fact 1); `list` and `status` must never guess. `DaemonStatus` already carries `pid`, `http_ingress_port`, `forward_port`, worlds (`crates/slipfork-edge/src/daemon.rs:43-102`). No CLI edits in this node.

# Design / Invariants

1. Extend `DaemonStatus` with `pub ingress: IngressInfo { addrs: Vec<SocketAddr>, port: u16 }` (the actually bound/`LISTEN_FDS`-adopted listeners, in adopt order; `port` must be validated present in `addrs`, fail closed) so a port-80 bind is visible and provable.
2. `WorldSummary` gains `pub parent: Option<WorldId>` (canonical world `None`, fork `Some(<project>)`) so `status` never string-splits a world id to draw its tree.
3. New `DaemonControl::world_processes(&WorldSelector) -> WorldProcessReport { lineage: WorldLineage, services: Vec<WorldServiceSummary> }` with `WorldServiceSummary { world, id, kind, state: ServiceState, pid: Option<u32>, port: Option<u16>, uptime_secs: Option<u64> }`, for components and `crun` backing services; unknown/destroyed world → typed error, never an empty list. One spelling only: `port` (never `readiness_port`), `services` (never `processes`), and `state` is the engine's `ServiceState` enum serialized.
4. Wire types are `serde` snake_case and additive; `--json` purity (stdout only) is the CLI's job, not the daemon's.
5. `crates/slipfork/src/daemon.rs` implements both over existing records/registry; no new filesystem, `/proc`, env, or clock read in the control path (the injectable clock is fine).
6. New `WorldLineage` **per world only** (there is no daemon-wide `DaemonStatus.lineage` — a world's header must never show another world's build facts): `rootfs {digest, cache: hit|miss, millis: Option<u64>}`, `base {key, millis: Option<u64>}`, `seed {digest, rebuilt: bool, millis: Option<u64>}` and `services: Vec<{id, cache: hit|miss|miss_offline, millis: Option<u64>}>`. This is the ONLY source of the `up` lineage line, the `ps`/`status` digests and `up -v` per-node detail (locked decision 8 in `docs/design/dx-0.1.0-ux.md`); unknown facts are `None` and render `-`, never `0` and never an invented value.
7. No globals, no `match engine_name`, no downcast, no new dependency.

# Acceptance

```console
$ cargo test -p slipfork-edge --lib
... test result: ok
$ cargo test -p slipfork-edge --test critical_edge
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ ! grep -rn '/proc' crates/slipfork/src/daemon.rs && echo no-proc
no-proc
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_edge` proves two worlds never share rows, a stopped service keeps its row, and `ingress {addrs, port}` round-trips a `:80` and an ephemeral bind, with `parent` present on forks and `None` on the canonical world.
- `critical_edge` also proves `WorldLineage` round-trips with every `Option` fact present and absent (`null` stays `null` through serde; no `0`/empty-string coercion), and that `world_processes` retires `WorldGone` for an unknown world instead of an empty report.

# Split with `M3.28.4` (binding)

This node owns the **wire shape and the daemon method**: `DaemonStatus.ingress: IngressInfo`, `WorldSummary.parent`, `WorldProcessReport`, `WorldServiceSummary`, `WorldLineage`, and `DaemonControl::world_processes` implemented over the daemon's own world registry and `ServiceHandle` states. The `pid`, `uptime_secs`, serving-port and every `WorldLineage` **value** are filled by `M3.28.4`'s `WorldEngine::world_processes`/`world_lineage` accessors (engine paths belong to that node); until then they are `None` and render as `-`, never `0`. Both nodes ship inside `M3.28`, so no placeholder survives the epic gate.
