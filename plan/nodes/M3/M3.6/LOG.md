# Log M3.6

## 2026-11-16T12:33 [conductor]

created (task) Restrict IncusEngine to LXC containers only: delete QEMU path & stateful-RAM promise

## 2026-11-16T12:33 [conductor]

OWNER RULING: Slipfork targets Incus LXC system containers ONLY. QEMU/VM path and stateful-RAM snapshots are deleted from the design of record (untestable on our only live target: DO droplets have no nested KVM; warm-pool path already measured at ~118 ms/test). Every engine keeps failing closed on SnapshotMode::StatefulRam.

## 2026-11-16T12:35 [conductor]

ADDENDUM (owner question: can LXC give millisecond fork?): make the fork vocabulary precise in ADR-0002/ARCHITECTURE/VISION instead of a blanket 'fork in milliseconds'. Three distinct mechanisms, each with its own measured budget: (1) STATE fork = ZFS/Btrfs dataset snapshot + 'incus copy' = O(1) metadata, target <=50 ms regardless of PGDATA size (to be measured on the DO VM in M3.3); (2) PROCESS warmth = a pool of pre-booted containers so container boot (~200-400 ms) is never on the critical path, plus 'fork()' zygotes inside the world (<1 ms) to skip framework imports; (3) RAM-state resume (CRIU) is NOT a target and must not be implied. Any roadmap/vision sentence promising 'millisecond fork' must state which of the three it means.

## 2026-11-16T13:48 [conductor]

LIVE MEASUREMENTS on the ams3 DO VM (Ubuntu 24.04, ams3, s-4vcpu-8gb, ZFS pool on a 100 GiB DO volume, Incus 6.x): (1) zfs clone of a full container dataset = 39 ms -> this is the real 'millisecond state fork' number; (2) incus snapshot create = 140 ms; (3) incus copy <instance>/<snapshot> <new> = 660-950 ms -> a full world fork is sub-second, NOT milliseconds, so the pre-warmed pool is mandatory for the test path and the <=50 ms target applies to the ZFS dataset clone only; (4) incus exec per-command overhead = 90 ms (batch execs in the harness); (5) first incus launch = 3 s including a 2.26 GB image pull. Also: two do_vm.py provisioning gaps found live -> the 'default' profile had no root disk device (incus launch failed with 'No root device could be found'; fixed by 'incus profile device add default root disk path=/ pool=default') and the tool has no --identity flag (SSH relies on ambient ~/.ssh keys, so it fails closed on hosts without one).

## 2026-11-16T13:58 [conductor]

OPTIMISATION INSIGHT from the ams3 numbers (incus copy 660-950 ms vs zfs clone 39 ms): the ~600-900 ms delta is daemon/DB bookkeeping, not storage. Because our world invariant already makes the rootfs READ-ONLY and every scratch path tmpfs (/tmp,/var/tmp,/run,/dev/shm), a container is STATELESS: only the declared state dirs (pgdata/redis/s3) change. So the fast path is 'fork the state, not the container': keep W long-lived slot containers and, per test, zfs-clone only the state datasets (~39-80 ms) + restart the services in the slot. That removes incus copy and incus start from the critical path entirely. Caveat to enforce: any component writing outside its declared state dir would leak across tests -> non-declared writes must fail or be discarded, and that needs a critical test. Secondary levers: pool of running slots; parallel zfs clone refill; single-snapshot template + --instance-only for the worktree copy path; batch incus exec (90 ms per command).

## 2026-11-16T16:53 [conductor]

status ready -> superseded: Superseded by total Option A Btrfs-only wipe (deleting Incus, XFS FICLONE, and KVM/QEMU completely)
