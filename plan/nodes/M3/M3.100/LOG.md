# Log M3.100

## 2026-11-24T02:18 [conductor]

created (task) Reap slipfork-world-init processes left by a healed/restarted pod

## 2026-11-24T02:18 [conductor]

status ready -> active

## 2026-11-24T02:48 [expert]

Root cause: (1) M3.83 added step 3b (buildfail) in critical_world_journey.rs without a trailing down; WorldRuntime::new eagerly boots WorldSupervisor (unshare + slipfork-world-init, adjacent PIDs 4201/4202 both matching b'slipfork-world-init' in /proc/<pid>/cmdline) and supervise.rs published even 0-pod cold image-build failures as World::Live parked in await_exit_and_cleanup. (2) __supervise calls setsid() (session == supervisor_pid) while WorldSupervisor::boot and WorldEgress::attach spawn unshare/slipfork-world-init and slirp4netns in .process_group(0) (pgid != supervisor_pid), so daemon kill_group(supervisor_pid) on abort/rollback/timeout missed both helper groups and re-parented them to PID 1. Fixed via WorldRuntime::has_live_world + stop_and_cleanup on 0-pod boot failures, slipfork_world::reap_world_tree (reaping session == supervisor_pid + exact --pods <pods_dir> argv and polling /proc empty), BootSession/down/reconcile wiring, and InitWorld::restart pod reap/status hygiene. Verified on 165.232.86.174 loop-Btrfs (/mnt/slipfork-live): critical_world_journey 2/2, critical_fork_live 3/3, critical_pod_runtime 1/1, pod_control_lab 15/15, critical_status_live::live_up_never_reports_ready_for_a_dead_service 1/1.

## 2026-11-24T02:54 [lead]

verify: fmt+clippy -D warnings clean, cargo test --workspace 0 failed, live target compiles (host is xfs/non-root; loop-Btrfs proof on VM per expert log), guard OK, critical_guard OK, validate OK

## 2026-11-24T02:54 [lead]

status active -> done
