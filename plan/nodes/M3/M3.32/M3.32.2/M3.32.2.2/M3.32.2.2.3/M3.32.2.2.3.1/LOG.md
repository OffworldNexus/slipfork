# Log M3.32.2.2.3.1

## 2026-11-19T19:03 [lead]

created (task) Explicit Interactive/Exec entry kind in wire + engine; delete INTERACTIVE_ENTRY_SHIM

## 2026-11-19T19:03 [lead]

status ready -> active

## 2026-11-19T19:03 [lead]

attempt 1/3 started

## 2026-11-19T19:03 [lead]

base 2cb20138018a1daf9abf0b2b562e1547d0e51c8c

## 2026-11-19T19:04 [lead]

edited fields

## 2026-11-19T19:04 [lead]

lead: widened to the entry producer/consumer (entry.rs, process.rs, daemon/protocol.rs)

## 2026-11-19T19:26 [implementer]

do_vm test RC=0 (12 suites): critical_btrfs_live 1, critical_engine 21, critical_oci_runtime 4, critical_pod_runtime 1, critical_edge_live 3, critical_btrfs_live(cli) 7, critical_cli 24, critical_l0_wiring 8, critical_slug 8, critical_ospp_daemon 7, critical_world_journey 7, critical_vcs_remote 7; EntryKind{kebab-case Exec|Interactive} on ExecTargetRequest/WorldExecTarget; WorldEngine::entry_command_for; BtrfsEngine bakes script -qec PTY relay; INTERACTIVE_ENTRY_SHIM + /bin/sh -c deleted; CLI shell=Interactive, exec/db/redis/hook=Exec; bridge entry_command/exec_in_world kept

## 2026-11-19T19:26 [lead]

status active -> review

## 2026-11-19T19:26 [lead]

edited fields

## 2026-11-19T19:26 [lead]

status review -> ready: triad: 1 declarative BLOCKER (test paths) + 4 MAJOR (silent default bridge, Interactive fallback, unused wire field, hardcoded script path)

## 2026-11-19T19:26 [lead]

attempt 2/3 started

## 2026-11-19T19:26 [lead]

status ready -> active

## 2026-11-19T19:48 [implementer]

attempt2: do_vm RC=0 (12 suites): btrfs_live 1, engine 21, oci_runtime 4, pod_runtime 1, edge_live 3, btrfs_live(cli) 7, cli 24, l0_wiring 8, slug 8, ospp_daemon 7, world_journey 7, vcs_remote 7; required entry_command_for; Interactive strict singleton + script probe/quoting (exec '<prog>'); consumes_target_argv single owner; wire target entry_kind removed; shell requires stdin+stdout TTY; protocol split (374/139 LOC)

## 2026-11-19T19:48 [lead]

status active -> done
