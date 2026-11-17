---
id: M3.28.4
title: "Restart vs up semantics, the port-less origin renderer, and the engine process-tree accessor"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.3]
paths:
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-world/src/**"
  - "crates/slipfork-edge/src/**"
  - crates/slipfork/src/daemon.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T16:32"
---

# Goal

Lock the daemon/engine semantics behind the new verbs — ONE canonical answer for what `restart` re-runs versus `up`'s idempotency — plus the port-less `.localhost` origin rendered at `:80`, plus the engine accessor that fills the process-tree rows (`pid`, `uptime`, serving port).

# Context

Facts 3 and 12: `restart` did not exist, and re-running `up` printed the same URL with no verdict. `WorldOrigin::local` already renders `http://<host>` with no port at 80 (`crates/slipfork-world/src/lib.rs:298-312`); `M3.28.3` shipped the `DaemonControl` rows with `pid`/`uptime_secs` as `None` placeholders, which this node fills from the engine.

# Design / Invariants

1. **One canonical restart contract** (written into the type docs): `restart` = stop components and `crun` backing services while keeping the worktree and CoW state, then re-run the full boot pipeline for that world — hooks, app process, containers, readiness gates. `up` stays idempotent: a running world returns `changed: false` and touches nothing. No third path exists.
2. `WorldUpSummary.changed` (edge) is set truthfully by both paths; `restart` on a `Failed` world re-boots (the rescue path), on a never-booted world fails closed naming `slipfork up`.
3. New `WorldEngine::world_processes(&WorldHandle) -> Vec<WorldServiceInfo { id, kind, pid, readiness_port, started_at }>` over the engine's own supervision records (`ServiceProcess`/`ContainerProcess`), deterministic order; destroyed/foreign handle → `EngineError::WorldGone`. `started_at` is an injected-clock timestamp so uptime is computed without a second clock read.
4. `WorldOrigin::local(&route, ingress_port)` is the single origin renderer: assert (tests) `:80` → port-less, `:1..=65535` → `:<port>`, and that any route with a `snow.yml` domain still fails closed. No other module may format an origin URL.
5. Keep the `StubWorldEngine`/test doubles compiling in the same sweep; no new dependency, no `unsafe`, no global.

# Acceptance

```console
$ cargo test -p slipfork-engine --lib
... test result: ok
$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
... test result: ok
$ cargo test -p slipfork-world --test critical_world
... test result: ok
$ ! grep -rn 'format!("http://' crates/ --include=*.rs | grep -v crates/slipfork-world/ && echo one-origin-renderer
one-origin-renderer
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- Engine tests prove a stopped service keeps its row with `pid: None`, two worlds never share rows, and `started_at` is monotonic from the injected clock.
- `critical_world` pins port-less-at-80 and `:<port>`-otherwise, with the existing default-ingress failure unchanged.
