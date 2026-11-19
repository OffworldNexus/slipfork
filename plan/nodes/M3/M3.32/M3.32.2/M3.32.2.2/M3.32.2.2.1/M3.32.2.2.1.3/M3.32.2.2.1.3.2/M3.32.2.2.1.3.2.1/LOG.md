# Log M3.32.2.2.1.3.2.1

## 2026-11-19T13:50 [lead]

created (task) Init + coordinate flip: drop pivot_root/overlay, publish namespaces.json, host-coordinate bundles, pod-backed legacy verbs

## 2026-11-19T13:50 [lead]

status ready -> active

## 2026-11-19T13:50 [lead]

attempt 1/3 started

## 2026-11-19T13:50 [lead]

base 67386f22fc9de75f7a9cbe912f184caa7b5803e6

## 2026-11-19T14:04 [lead]

edited fields

## 2026-11-19T14:04 [lead]

unblocked: apparmor profile now in scope (coordinate flip moved crun to a host rootfs path)

## 2026-11-19T15:00 [lead]

status active -> blocked: architect: secret env in OCI config leaks to disk (oci/ + oci-run/ copies)

## 2026-11-19T15:31 [lead]

status blocked -> ready: architect decision: world-mntns tmpfs over ctl/{oci,oci-run}

## 2026-11-19T15:31 [lead]

attempt 2/3 started

## 2026-11-19T15:31 [lead]

status ready -> active

## 2026-11-19T15:31 [lead]

architect: mount per-world mntns tmpfs over ctl/{oci,oci-run}; translate host writes via /proc/<init>/root; restore WorldProcess respawn for update_service_env

## 2026-11-19T17:40 [implementer]

do_vm test RC=0: critical_btrfs_live 1, critical_engine 21, critical_oci_runtime 4, critical_edge_live 3, critical_btrfs_live(cli-crate) 7, critical_cli 24, critical_l0_wiring 8, critical_slug 8, critical_ospp_daemon 7, critical_world_journey 7, critical_vcs_remote 7; critical_pod_bundle 1 (local, alpine no-Btrfs) incl. new-join contract; lint/fmt clean

## 2026-11-19T17:41 [lead]

edited fields

## 2026-11-19T17:41 [lead]

lead: new pod suite must run in the VM gate; do_vm.py SUITES in scope

## 2026-11-19T18:01 [implementer]

do_vm test RC=0 (12 suites): critical_btrfs_live 1, critical_engine 21, critical_oci_runtime 4, critical_pod_runtime 1, critical_edge_live 3, critical_btrfs_live(cli) 7, critical_cli 24, critical_l0_wiring 8, critical_slug 8, critical_ospp_daemon 7, critical_world_journey 7, critical_vcs_remote 7; pod suite added to do_vm SUITES; lint/fmt/pod_bundle clean

## 2026-11-19T18:01 [lead]

status active -> review

## 2026-11-19T18:03 [lead]

status review -> ready: triad: 3 BLOCKER (shared ns doc, script not staged, IPC join unproven) + 12 MAJOR

## 2026-11-19T18:03 [lead]

attempt 3/3 started

## 2026-11-19T18:03 [lead]

status ready -> active

## 2026-11-19T18:23 [implementer]

do_vm test RC=0 (12 suites): critical_btrfs_live 1, critical_engine 21, critical_oci_runtime 4, critical_pod_runtime 1, critical_edge_live 3, critical_btrfs_live(cli) 7, critical_cli 24, critical_l0_wiring 8, critical_slug 8, critical_ospp_daemon 7, critical_world_journey 7, critical_vcs_remote 7; namespaces.json now serializes PodNamespaces; RuntimeKind removed; world_mount_path removed; pod IPC-join asserted; setsid-escaped descendant reaped

## 2026-11-19T18:23 [lead]

edited fields

## 2026-11-19T18:23 [lead]

attempt 1/5 started

## 2026-11-19T18:23 [lead]

status active -> active

## 2026-11-19T18:23 [lead]

final structural cleanup: container.rs split, WorldProcessBundle, unexport write_pod_bundle

## 2026-11-19T18:42 [implementer]

do_vm test RC=0 (12 suites): critical_btrfs_live 1, critical_engine 21, critical_oci_runtime 4, critical_pod_runtime 1, critical_edge_live 3, critical_btrfs_live(cli) 7, critical_cli 24, critical_l0_wiring 8, critical_slug 8, critical_ospp_daemon 7, critical_world_journey 7, critical_vcs_remote 7; container.rs split into crun.rs (265 LOC) + container.rs (308 LOC); WorldProcessBundle builder replaces 10-arg write_process_bundle; item 3 (unexport write_pod_bundle) blocked by the Docker-only critical_pod_bundle test

## 2026-11-19T18:42 [lead]

arch PASS; defer one-line doc on write_pod_bundle (bundle-synthesis-only) to M3.32.2.2.1.3.2.2

## 2026-11-19T18:42 [lead]

status active -> done
