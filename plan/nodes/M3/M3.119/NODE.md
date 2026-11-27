---
id: M3.119
title: "BuildGraph + SnapshotStore: one desired-graph reconcile/gc manager for Btrfs snapshots"
kind: task
status: done
owner: architect
priority: 3
depends_on: []
paths: ["specs/03-under-the-hood.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T16:55"
updated: "2026-11-24T17:09"
---

# Problem

Snapshot/cache ownership is split: `StepCache` (content-keyed sealed step rootfses, no GC), `ImageCache` (sealed images + pulled bases, fork adoption), `StampStore`/`WorldStampStore` (world epochs), `BtrfsEngine` (subvolume primitives), and `up_plan`'s own Forkfile-only fingerprint. Consequences: `up` returns Idle over a stale image, the sealed stamp ignores Dockerfile/context changes, `restart` never reconciles, nothing prunes unreachable snapshots.

# Design (normative)

- **`BuildGraph`** (pure): from the compiled Forkfile + Dockerfiles. Nodes: `FROM`/pull (resolved base digest), `COPY`/`ADD` (context digest honouring `.dockerignore`; external refs by digest), `RUN` (command + env + args + parent), stage boundaries, final image; plus world data nodes (PG/Redis/S3). Content-addressed key = hash(parent key, kind, canonical instruction, inputs, arch, args, external/base digest). Edges = parent. A graph **head** is the root digest.
- **`SnapshotStore`** (owned by the composition root / world engine): stores immutable, sealed subvolumes keyed by node digest (`<store>/steps/<key>` etc.), with the existing `<key>.json` records. `reconcile(desired) -> {head, built, reused, pruned}`: topological waves (parallel within a wave), temp subvolume + seal + atomic rename, failure destroys only temps; never reuses an unsealed/half node.
- **Leases + GC:** `lease(world|fork) -> Lease` pins the world's head and data nodes; `gc(mark = live leases + desired head + retained failed-run logs)` destroys unmarked snapshots (bounded retention; never a leased node). Run after every reconcile.
- **Entry points:** `up`, `restart`, `docker build` build the desired graph and call `reconcile`; no separate up fingerprint/stamp; `Idle` iff desired head == leased head AND services healthy. `--no-cache` reconciles with all keys cold.
- **Failure atomicity:** a failed node leaves the previous leased head intact; the world only switches after the new head is fully sealed.
- **World data snapshots** are ordinary nodes leased by the world; forks lease the parent's data nodes and add their own.

# Acceptance

- `specs/03-under-the-hood.md` gains a normative section with the key grammar, reconcile/gc/lease semantics, entry points, failure atomicity, retention, and a 3–5 slice refactor plan (crate-ordered, ≤500 LOC files).
- The spec explicitly maps today's `StepCache`/`ImageCache`/`StampStore`/`up_plan` onto the new contract and what is deleted.
- No code in this node; a `planner` decomposes the slices after.
