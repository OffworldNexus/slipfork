---
id: M3.28.17
title: "Restart vs up: one boot pipeline, truthful WorldUpSummary.changed and restarted ids"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.28.4]
paths:
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/**"
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-engine/tests/**"
  - crates/slipfork/src/daemon.rs
  - ARCHITECTURE.md
attempts: 3
max_attempts: 4
escalation: none
created: "2026-11-17T19:46"
updated: "2026-11-17T22:13"
---

# Goal

One canonical restart contract across edge and daemon, with `up` staying idempotent: `restart` = stop components + `crun` backing services (worktree and CoW state kept), then run the **same** boot pipeline (hooks, app, containers, readiness gates). `WorldUpSummary.changed` tells the truth on both paths. This is the semantics half that `M3.28.4` (engine facts) deliberately does not own.

# Context

Split out of `M3.28.4` after attempts 1–2 hit their step budget on a node that carried both the engine accessors and the restart/`up` semantics. `M3.28.3` shipped the wire; `M3.28.4` fills the engine facts (`world_processes`, `world_lineage`, injected clock, `WorldLineage::unknown()` deleted). This node owns only the two verbs and the summary flag. `M3.28.8` renders the block and the `[restarted]`/`[no change]` line; `M3.28.7` renders status.

# Design / Invariants

1. **One pipeline, two verbs:** `up` boots a stopped **or failed** world through the boot pipeline and is idempotent when running (`changed: false`, nothing touched); `restart` stops components + `crun` services, then runs the same pipeline. No third path, no `restart: bool` threaded into a trait, no `down`-then-`up` composition in the client.
2. `EdgeRequest::Restart` / `DaemonControl::restart(&WorldSelector)` exists on the wire and is served by the daemon; `WorldUpSummary.changed` is set truthfully by both paths (`false` when the world was already running and nothing was rebuilt).
3. `restart` on a never-booted world fails closed with one actionable line naming `slipfork up`; `restart` on a `Failed` world re-boots (the rescue path).
4. `WorldUpSummary` carries `{ world, state, changed, restarted: Vec<String>, worktree, origin, lineage, services, resources, next }` with `restarted` = the ids of the components/services actually restarted (unique, joinable to `services[].id`), empty when `changed: false`.
5. No new ambient IO: the daemon still answers only from the engine + its own records; the CLI never stops a world itself.

# Acceptance

```console
$ cargo test -p slipfork-edge --lib
... test result: ok
$ cargo test -p slipfork-edge --test critical_edge
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ cargo test -p slipfork --lib restart
... test result: ok
$ ! grep -rn 'down.*then.*up\|restart: bool' crates/slipfork/src/daemon.rs ; echo one-pipeline
one-pipeline
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- Tests prove: `up` twice on a running world reports `changed: false` and performs no work; `restart` after a `snow.yml` edit re-runs hooks and the app and reports the restarted ids; `restart` on a `Failed` world re-boots; `restart` on a never-booted world exits 1 naming `slipfork up`.
- `RESULT.md` records which daemon/edge paths changed and the real command output.
