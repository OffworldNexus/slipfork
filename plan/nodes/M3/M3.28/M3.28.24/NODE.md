---
id: M3.28.24
title: "restart must apply an edit: stop the whole component (not its sh wrapper) and re-materialize an edited rootfs"
kind: task
status: review
owner: implementer
priority: 1
depends_on: [M3.28.17, M3.27]
paths:
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-engine/tests/**"
  - crates/slipfork/src/daemon.rs
  - "crates/slipfork/tests/**"
  - "support/demo/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
attempts: 4
max_attempts: 3
escalation: expert
created: "2026-11-18T03:20"
updated: "2026-11-18T15:39"
---

# Goal

Make `slipfork restart` actually apply an edit, proven live on `165.232.86.174` (`M3.28.13` §6). Two independent defects: the component's `sh -eu -c` wrapper is stopped while its `python3` child survives on the port, and the cached `L0` rootfs is reused so an edited app never reaches the running world.

# Context

`M3.28.13` verbatim: *"`restart` after app edit → old body served, component `exited`, `EADDRINUSE`"*.

1. **Stop depth.** Every `run_command` is wrapped as `sh -eu -c …` (`crates/slipfork/src/daemon.rs:2248-2253`) and `crates/slipfork-engine/src/service.rs::wait_for_inner_pid` captures only the first child, so stop signals `sh` and **orphans** `python3`. No suite noticed because none keeps a distinct `sh` parent alive while it serves.
2. **Stale rootfs.** `reboot()` re-resolves `snow`/plan/coords but reuses the cached rootfs, so an edited `Dockerfile`/stage input never rebuilds.
3. **The demo's app must not be baked in.** The owner edited `app/server.py` inside `.worktrees/tralala/` and ran `slipfork restart`. If the demo's `run_command` runs `/app/server.py` copied in by the `Dockerfile`, no restart can ever apply that edit without a Docker rebuild — the app must run from the **bind-mounted checkout**.

# Design / Invariants

1. **Own session, own group.** Every spawned `WorldProcess` runs in its own session/process group (`setsid` at spawn); `stop_service` / `stop_world_services` signal the **group** (`kill(-pgid, …)`), TERM → grace → KILL. That kills `sh -eu -c` *and* its `python3`.
2. **Reap what the engine owns.** If the supervised child exits but a descendant survives in the group, stop must still reach it; the "survived SIGTERM+SIGKILL" error names the surviving pid **and its state** (`D`/`R`/`Z`), and a `D`-state pid is retried with backoff instead of being declared unkillable after a bare 15 s.
3. **Edited stage inputs rebuild the rootfs.** The rootfs digest covers the app-stage inputs; an edited `Dockerfile`/copied input flips the lineage to `rebuilt` and serves the new content, an unchanged tree keeps `reused`. No unconditional rebuild, no stale build.
4. **The demo runs from the checkout.** `support/demo/snow.yml`'s component runs `app/server.py` **from the bind-mounted worktree** (component cwd/mount), so editing the file in `main` or in `.worktrees/<fork>` and running `slipfork restart` applies instantly with no Docker rebuild. `support/demo/Dockerfile` keeps only runtime deps (python), not the app source.
5. One code path: `boot` and `reboot` share the resolver; only the cache verdict differs.

# Acceptance

```console
$ cargo test -p slipfork-engine --lib
... test result: ok
$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- A protected test drives `restart` over a **`sh -eu -c` component that keeps serving**: after it, the old pid is gone (the new boot logs contain no `EADDRINUSE`) and the served body is the edited one; a second test edits a copied `Dockerfile` input and asserts the lineage reads `rebuilt`.
- `support/demo`'s app is edited in place and served after `restart` on the VM (`M3.28.13` re-runs this as step 6).
- `RESULT.md` records the live before/after from `M3.28.13` §6 plus the local transcripts.
