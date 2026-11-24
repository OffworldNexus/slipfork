---
id: M3.81
title: up must not print Ready when a service is failed (verify health before Idle)
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/lifecycle/up.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/src/daemon/rpc.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/tests/critical_status_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:14"
updated: "2026-11-24T00:33"
---

# Problem

`slipfork up` printed `✓ Ready -> http://test1.localhost` while `slipfork ps` immediately showed `web failed` (and the route `/ -> web failed`).

`Core::up_plan` (`crates/slipfork/src/daemon/lifecycle/up.rs:48-104`) returns `UpAction::Idle` (which the CLI renders as `✓ Ready`) when the registry entry is live, `pid_alive(supervisor_pid)` and `world_answers(&pods_dir)` hold, and the Forkfile fingerprint is unchanged. `world_answers` only proves the control socket answers; it never checks that the declared services/resources are actually running. So a crashed service is reported as a healthy world forever.

# Contract

- `Idle` is only valid when **every declared long-lived service and resource pod is running** (one-shot hooks that already exited successfully do not count as failed — reuse the canonical `ps` row semantics rather than inventing a second health model).
- If something is down: bring it back (preferred: restart the failed pod through the world control protocol) and print `✓ Ready` only after the rows are healthy. If it cannot be recovered, fail with the exact `1 line + next:` (`next: slipfork logs <service>`), never `✓ Ready`.
- No duplicate/contradictory state between `up` and the immediately following `ps`.
- `--json` mirrors the same decision.

# Pre-baked findings

- `up.rs:48-104` `up_plan`; the `Idle` return is at ~88, the `Reconcile` path below it.
- `crates/slipfork/src/daemon/lifecycle/ps.rs` / `daemon/ps.rs` already compute per-service `state` (`running`/`failed`) from the world; `world_rows(...)` is used by status. `up.rs` may call the same ps query (or `Request::List` + `slipfork_world::pod_statuses`, as `reconcile()` does) while deciding Idle.
- `crates/slipfork-world/src/port.rs` / `supervisor` expose pod status and restart; `reconcile()` shows the `Request::List` usage.
- Existing `critical_world_journey` has the lab harness for a real world + supervisor; add the case there (a service that exits right after boot), not a new slow journey.

# Acceptance

- Unit/lab proof: boot a world with a service that exits after start; `up` must not print `✓ Ready`; a second `up` either restarts it (then Ready) or fails with `next: slipfork logs <service>`. `up --json` carries the same verdict.
- `make check`; `cargo test -p slipfork --test critical_world_journey`; targeted `critical_status_live`.
- Live on `165.232.86.174`: a demo world whose `web` command exits; `slipfork up` reflects it truthfully and a subsequent `ps` agrees.
- Commit `[M3.81] ...`, done, no push.
