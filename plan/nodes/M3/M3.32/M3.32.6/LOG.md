# Log M3.32.6

## 2026-11-19T13:02 [planner]

created (task) Zero-mock lab E2E + live multi-service PTY proof on 165.232.86.174, demo left on :80

## 2026-11-19T13:02 [lead]

edited fields

## 2026-11-19T16:00 [lead]

edited fields

## 2026-11-19T16:03 [lead]

edited fields

## 2026-11-19T22:32 [lead]

status ready -> active

## 2026-11-19T22:32 [lead]

gate run 1: make check RC=2 — 2 rustdoc private-intra-doc-link errors (registry.rs ingress_table, projection.rs world_projection)

## 2026-11-19T22:32 [lead]

status active -> ready: held: M3.32.2.2.3.2 + M3.32.2.3 must land first (per-service crun pods)

## 2026-11-19T22:41 [lead]

edited fields

## 2026-11-19T23:06 [lead]

VM test RC=101: critical_btrfs_live ipc join publishes /proc/1/ns/ipc; leaked worlds on drop

## 2026-11-21T00:43 [conductor]

status ready -> superseded: Superseded by clean-sheet wipe & rebuild from specs/ (M3.37..M3.42)
