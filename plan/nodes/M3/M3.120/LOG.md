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

## 2026-11-24T21:44 [lead]

committed 7680850 [M3.120] slice 1: world snapshot graph (19 files, +2785/-21); node stays active for slices 2-4

## 2026-11-24T21:45 [lead]

status active -> active

## 2026-11-24T21:45 [lead]

attempt 2/3 started

## 2026-11-24T21:45 [lead]

slice 2 base 7680850406416b02a369cf0511b99ef1103e85cd

## 2026-11-24T22:06 [conductor]

edited fields

## 2026-11-24T23:47 [lead]

slice 2: image plan/materialize green — RefResolver (tag->digest, pinned unpack), per-COPY source Merkle, BuildPlanner+expand, serial store reconcile w/ rows, CacheMounts; steps.rs/run.rs/StepCache/StepLayout deleted; triad fixes: digest pin, shared process core, injected clock, ARG scope, metadata rows

## 2026-11-24T23:47 [lead]

slice 2 VM proof (private tree /mnt/sf-m3120, no do_vm sync, as slipfork user w/ XDG_RUNTIME_DIR): critical_dockerfile_live 6/6, critical_snapshot_graph 1/1, btrfs addendum test 1/1; CLI guard: up built->cached, docker build warm 4/0, COPY edit 3 misses, .git-only 0, Dockerfile edit 1, dev /project live, fork rows all <1ms + clean checkout, up --no-cache built; make check rc=0

## 2026-11-24T23:49 [lead]

addendum: fork checkouts never contain .forks/.slipfork-store (engine owner, one layout::is_managed_entry predicate shared with copy_checkout); real-Btrfs test proves two existing forks + seeded stale partial -> third checkout clean, parent intact, fork_checkouts empty on the checkout, subvolume path; VM live test ok 0.11s

## 2026-11-24T23:54 [lead]

committed slice 2 b209bbc (31 files, +2576/-1066) + addendum a419321; make check rc=0 after both; node stays active for slices 3-4

## 2026-11-24T23:55 [lead]

status active -> active

## 2026-11-24T23:55 [lead]

attempt 3/3 started

## 2026-11-24T23:55 [lead]

slice 3 base a419321

## 2026-11-25T00:04 [lead]

slice 3a landed: image BootState = Cached iff cache_misses==0 (zero executed steps never 'built'; warm rebuild labels cached, Idle prints no image row), unit test zero_executed_steps_is_cached_not_built; specs/03 §6.1 COPY --from=source-key rule, §6.2 planned-digest unpack + externals pre-pull note, §6.7 row 3 daemon owns pulls; make check rc=0; say nothing about a cutover that is not committed
