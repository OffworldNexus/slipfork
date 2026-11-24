---
id: M3.120
title: "BuildGraph + SnapshotStore refactor (4 slices): reconcile, lease, GC"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.117, M3.115, M3.118, M3.114, M3.111]
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork-image/**"
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/world/**"
  - "specs/02-cli-ux.md"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T17:09"
updated: "2026-11-24T21:13"
---

# Goal

Implement `specs/03` §6 "One Snapshot Graph": a daemon-owned `SnapshotStore` and a pure `BuildGraph` so `up`, `restart`, `docker build` and `docker pull` all reconcile one desired head; `Lease` pins live worlds; GC mark-and-sweeps unreachable snapshots; fingerprints and separate caches disappear.

# Slices (crate order, each file ≤500 LOC)

1. `slipfork-world` `snapshot/{graph,key,store,slot,gc,lease}.rs` + PG seed node: key grammar, DAG waves, head, temp→seal→rename, pins, GC (24h grace), fail-closed lease rule; pure unit tests + real-Btrfs lab test.
2. `slipfork-image` `plan/{refs,planner,sources}.rs` + `builder/materialize.rs`: per-COPY source digests (after `.dockerignore`), move `ImageCache` and CLI `docker build` call sites, delete `steps.rs`.
3. `slipfork` `daemon/reconcile/`: move `up`, `restart`, `docker build`, `docker pull` into the daemon; the supervisor consumes sealed heads; delete `ImageCache`.
4. `boot/` + `up_plan`/`fork`: `Lease` replaces stamps; `up` Idle iff desired head == leased head and services healthy; fork keys over parent images; GC after reconcile; `--no-cache` = cold graph; delete fingerprints; split `schedule.rs`/`up.rs`; update `specs/02`, `TESTING_PLAN.md`.

# Behaviour change to document

A fork whose image bakes a world URL (e.g. `VITE_CDN_URL`) still boots the parent image initially, then its next `up` rebuilds only the steps using that value (today it keeps the parent's URL forever).

# Acceptance

- Each slice: Triad gate, live Btrfs proof, ≤500 LOC files; final: full workspace + `make live-test` on the VM; one `[M3.120]` or per-slice commits, no push.
