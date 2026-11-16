# Log M3

## 2026-11-14T22:03 [strategist]

created (milestone) Polyglot Parallel Snapshot-DAG Test Orchestrator (0.3.0)

## 2026-11-16T16:53 [conductor]

OWNER RULING (SCORCHED EARTH — OPTION A BTRFS-ONLY): (1) Option A (unified Btrfs subvolumes + Linux namespaces `userns+netns+ipcns+pidns+mntns` + `pivot_root` + `crun`) is the SOLE `WorldEngine` (`BtrfsEngine`). (2) Wipe EVERYTHING that isn't Option A from code, docs, ADRs, CLI, tools, and tests — delete `IncusEngine`, delete XFS `FICLONE` `ReflinkNetnsEngine`, delete ZFS/Incus from `do_vm.py` (retarget `do_vm.py` to Btrfs `-o user_subvol_rm_allowed,noatime` + `crun` + AppArmor `userns` + persistent SSH identity), zero backward compat, zero fallbacks. (3) Then deliver M3 (`0.3.0`) `slipfork test` polyglot orchestrator (`RunnerProtocol v1`, rolling warm Btrfs world pool, typed coverage, `pytest-slipfork` + `CommandTestAdapter`).
