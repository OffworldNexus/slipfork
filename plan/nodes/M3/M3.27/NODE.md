---
id: M3.27
title: CLI critical suites leak one Btrfs world subvolume per test; reap worlds as well as processes
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.19]
paths: ["crates/slipfork/tests/**", "crates/slipfork-engine/src/**", crates/slipfork/src/daemon.rs]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-17T14:21"
updated: "2026-11-18T22:19"
---

# Goal

Stop the `slipfork` CLI critical suites leaking one Btrfs world subvolume per test, and make the world reaper cover worlds, not only processes.

# Context (M3.19 attempt 2, live VM)

12 of 14 `critical_cli` tests never call `slipfork down`; `Daemon::shutdown` SIGKILLs its child daemon so the engine's `Drop` reclaim never runs, and each test leaves one world subvolume inside its per-test temp state dir (27 accumulated across two full runs). `TempDir` cannot delete the read-only sealed base, so the whole temp dir survives on disk after the suite.

# Design / Invariants

1. Every test that boots a world must tear it down in-process (`down`/`destroy_world`) before the daemon is killed, or the harness must destroy the worlds it booted before `Daemon::shutdown` reaps the process.
2. The M3.24 reaper (`reap_world_init_processes`) gains a world counterpart that asserts no world subvolume survives under the suite's state dir (query the injected `SubvolumeDriver`, never a hard-coded path).
3. No silent skip and no weakened assertion: if a world cannot be destroyed, the suite fails with the surviving subvolume names.
4. No production behaviour change.

# Acceptance

```bash
cargo test -p slipfork --test critical_cli -- --test-threads=1
cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1
btrfs subvolume list ~/.cache/slipfork-cli-tests | grep -v /snapshots/   # empty after the suites
make check
```
