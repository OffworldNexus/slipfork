---
id: M3.100
title: "Reap slipfork-world-init processes left by a healed/restarted pod"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/world/**"
  - "crates/slipfork-world/**"
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 0
max_attempts: 3
escalation: expert
created: "2026-11-24T02:18"
updated: "2026-11-24T02:54"
---

# Problem

CI `live-test` (`world_up_restart_down_journey`) now fails at `critical_world_journey.rs:1916`:

```
processes survived teardown (needle, pid): [("slipfork-world-init", 4201), ("slipfork-world-init", 4202)]
```

The journey boots a world with a service that dies, then `up` (M3.81 heal), `restart`, `down`; two `slipfork-world-init` processes outlive every teardown. Before M3.80/81/83 the same test was green on this loop-Btrfs job.

# Contract

- `down` (and failed-`up` rollback, fork teardown) leaves **zero** `slipfork-world-init` processes for that world, including any spawned by a heal/restart path.
- Keep the existing `-pgid` discipline; do not add a test-only sweep or a mock. A targeted sweep of orphaned `slipfork-world-init` whose cmdline/env names the world's pods dir is acceptable only if the process tree genuinely escapes the supervisor's group — explain why in the node log.
- The heal path must register/track the pod it restarts so teardown knows about it.

# Evidence

Full CI log: run 45303305688 job 138140919468. The test also passes locally on non-Btrfs refusal paths, so the proof must run in CI's loop-Btrfs job (or the VM loop).

# Acceptance

- `make live-test` (CI's loop-Btrfs job) green: no `slipfork-world-init` survives; `critical_world_journey` 2/2.
- `make check`, `guard`, `plan validate`.
- Commit `[M3.100] ...`, done, no push.
