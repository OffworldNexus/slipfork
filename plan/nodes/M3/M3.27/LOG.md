# Log M3.27

## 2026-11-17T14:21 [lead]

created (task) CLI critical suites leak one Btrfs world subvolume per test; reap worlds as well as processes

## 2026-11-17T21:00 [lead]

Field evidence from the M3.28 epic (2026-11-17): under host load 16-19 (a foreign cargo test in D state), 'make check' fails non-deterministically with 'world init pid <n> survived SIGTERM and SIGKILL for 15.0s' across *different* tests (one_url_per_world, forward_bridge, secret_set, logs_follow, hook_failure). Three runs: 6 failures, then 2, then 0 once load dropped to 6.5 — the signature predates M3.28.4. Two robustness gaps to fix here: (1) teardown calls a pid 'survived' while it may be in D state and unkillable; report the actual state and retry with backoff instead of failing fast, (2) the 15s SIGTERM+SIGKILL budget is too tight on a loaded host and should be injectable/derived rather than a constant. This node is now the fixed owner of that flake class.

## 2026-11-18T00:19 [lead]

followup from M3.28.20: on a Btrfs host ensure_daemon spawns a real slipforkd child that currently outlives the test binary (no Drop guard). Teardown must also reap the harness-owned daemon, not only world-init processes and subvolumes.

## 2026-11-18T05:07 [lead]

status ready -> active

## 2026-11-18T05:07 [lead]

attempt 1/3 started

## 2026-11-18T05:07 [lead]

base 9a79afc81a20eb5dc0a911525a82f6f0aa673df6

## 2026-11-18T11:30 [lead]

attempt 1 report: done. Reaper covers worlds (injected SubvolumeDriver) + world-init + harness daemon Drop guard; ReapBudget derives from load; /proc state reported; D-state backoff. Host is XFS without btrfs-progs, so the btrfs-grep acceptance is deferred to the VM run in M3.28.13.

## 2026-11-18T11:30 [lead]

status active -> review

## 2026-11-18T11:35 [lead]

Quad gate attempt 1: review-legality FAIL, review-ioc FAIL, review-cruft FAIL (merged MAJORs). Root finding (legality+ioc+cruft agree): substrate_driver() re-derives the daemon's SubvolumeDriver from a host probe at 4 call sites instead of capturing the adapter each backend actually injects; on a Btrfs host the IN-PROCESS backend would wrongly inject KernelBtrfsDriver, and on the child branch the adapter is a second instance. Fix: one captured substrate per Daemon (Child => KernelBtrfsDriver, InProcess => TestTreeDriver), driver accessor for suite-level asserts, delete substrate_driver and the non-_with reap wrapper. Also: ensure_daemon must return Daemon (the Option/daemon_is_served early return is dead compat); capture ReapBudget::current_host() once at Daemon construction and make reap_world_init_processes_within the only entry (no ambient /proc/loadavg read inside shutdown); factor one reap_world_residue(processes->then->subvolumes) used by shutdown and both asserts; nested 'snapshots' skipped by name instead of state_dir.join(SNAPSHOTS_DIR); drop the redundant pre-handout guard, the dead .max(), the hand-rolled EPERM, the duplicated world fixture; fix RESULT.md over-claims (a reaped-but-undeletable world is the only failure; the reaper destroys survivors rather than proving survival) and the delta means the child-branch proof is only real on a Btrfs host.

## 2026-11-18T11:39 [lead]

attempt 2/3 started

## 2026-11-18T11:39 [lead]

status review -> active

## 2026-11-18T11:57 [lead]

attempt 2 report: done. All 4 MAJORs + 7 MINORs applied; make check exit 0.

## 2026-11-18T11:57 [lead]

status active -> review

## 2026-11-18T12:02 [lead]

status review -> done

## 2026-11-18T12:10 [lead]

Quad gate attempt 2: review-legality PASS, review-ioc PASS, review-cruft PASS (MINORs only), reviewer FAIL — reopened. The reviewer reproduced the LOG:9 flake class on an UNMODIFIED production path: 'cannot destroy the base seed world: world init pid N survived SIGTERM and SIGKILL for 15.026s' from crates/slipfork-engine/src/btrfs_engine.rs (WORLD_STOP_TERM_GRACE 5s + WORLD_STOP_KILL_GRACE 10s, no /proc state, no D-state backoff, no injectable budget). 3 of 4 suite runs red at load 6-17/8 CPUs. The harness ReapBudget covers only the test-side reaper, so attempt 3 must give reap_world_init the same state-bearing, D-state-backoff treatment. Attempt 3 is queued after M3.28.24 because that node is simultaneously reworking the same stop/teardown region (setsid + group signalling + /proc state); paths must widen to crates/slipfork-engine/src/** + crates/slipfork/src/daemon.rs. RESULT.md:182-194 and LOG over-claim the flake class and must record these red runs verbatim.

## 2026-11-18T12:10 [lead]

status done -> ready

## 2026-11-18T12:10 [lead]

edited fields

## 2026-11-18T22:19 [planner]

status ready -> done: Landed in HEAD (cdca7d3): CLI critical suites reap worlds and process groups.
