# Log M3.120

## 2026-11-24T17:09 [conductor]

created (task) BuildGraph + SnapshotStore refactor (4 slices): reconcile, lease, GC

## 2026-11-24T17:09 [conductor]

status ready -> ready

## 2026-11-24T17:51 [conductor]

OWNER: no leftover spaghetti. This refactor is deletion-first: remove steps.rs, ImageCache, StampStore/WorldStampStore, up_plan::fingerprint and inputs_predate in the slices that replace them; no compatibility shims, no dual cache paths, no old symbols left behind. Gate adds a grep for the deleted types/paths and the arch reviewer signs off on one SnapshotStore owner.

## 2026-11-24T21:03 [conductor]

Owner decision (2026-11-24): forks stay plain CoW checkouts. Do NOT convert fork projects into linked git worktrees. Rationale: git worktree add requires an empty/new target and git worktree repair only fixes an existing linkage, so adopting a CoW snapshot means hand-fabricating .git/worktrees/<name> admin files (HEAD/commondir/gitdir) plus per-file index rebuild; coupling forks to the parent's .git also means a dropped parent breaks its forks. Btrfs CoW already dedups the copied .git, and worktrees add no diff/merge ability beyond shared refs. Middle ground if asked later: a slipfork-side fork listing (status/fork --list), not git-native registration.

## 2026-11-24T21:12 [conductor]

status ready -> active

## 2026-11-24T21:13 [lead]

status active -> active

## 2026-11-24T21:13 [lead]

attempt 1/3 started

## 2026-11-24T21:13 [lead]

base 0e03d00aaa4c413bbe3583dbe4116e5378ecc15e

## 2026-11-24T21:44 [lead]

slice 1: snapshot/{key,graph,lease,slot,gc,store} green — key grammar, waves/head, record-only image records, pins=closure, per-key singleflight, 24h mark-sweep, fail-closed lease; triad BLOCKER (record closure) fixed in 1 pass; critical_guard violation was M3.130's (authorized)

## 2026-11-24T21:44 [lead]

VM live proof (private tree /mnt/sf-m3120, no sync): real-Btrfs critical_snapshot_graph ok (1 test, 1.23s) on /mnt/slipfork-live; make check cargo gates all green (48 suites), js-test blocked only by a pre-existing stale daemon (pid 958964), unrelated to this slice
