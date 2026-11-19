# Log M3.2.13

## 2026-11-16T16:59 [planner]

created (task) Snapshot + rootfs/OCI cache GC in slipfork prune (dry-run, live-world safe, idempotent)

## 2026-11-16T21:46 [lead]

REQUIREMENT (M3.10 ioc gate): the state-root GC must EXCLUDE ~/.cache/slipfork/do-vm/ (harness-owned persistent SSH identity + known_hosts). Never collect it, and assert that in a GC test.

## 2026-11-18T22:19 [planner]

status ready -> superseded: Obsolete pre-CBOR test runner draft; STP v1 belongs to 0.2.0
