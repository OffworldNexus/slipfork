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
