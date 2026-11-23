---
id: M3.50.10
title: "Stabilize real-PTY live journey: Forkfile vanishes between write and PTY up on the lab VM"
kind: task
status: done
owner: expert
priority: 5
depends_on: [M3.50.4]
paths:
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/commands/streams.rs
  - "crates/slipfork-world/src/bin/world_init/main.rs"
  - .opencode/LESSONS.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T15:45"
updated: "2026-11-22T16:18"
---

# Goal
`cargo test -p slipfork --features live --test critical_status_live -- --test-threads=1` must pass on the lab VM in one shot, proving the DECSTBM panel + orphaned-pod reconcile on real Btrfs.

# Context
Product flows verified by hand on the VM (fresh project, absolute paths): first `slipfork up` converts the checkout in place, a edited Forkfile reconciles, a hook Forkfile boots. But the consolidated test fails at step 4 with `error: cannot read Forkfile: No such file or directory` right after `fs::write(project/Forkfile, HOOK_FORKFILE)` + `assert!(is_file)`, and `run_pty` then spawns with `current_dir(project)`. Earlier symptoms: 0.38s dual failures when each test had its own runtime (only one daemon can own :80), and a `0/0` divide-by-zero (fixed). Suspect the in-place checkout conversion (`crates/slipfork-world/src/engine/convert.rs:20` removes + renames the project dir) racing a *previous* supervisor/test guard, or a stale shared daemon.

# Acceptance
- Root cause identified (not masked) and fixed in the harness and/or product.
- `timeout 600 cargo test -p slipfork --features live --test critical_status_live -- --test-threads=1` prints `3 passed` on the VM; report the duration.
- `cargo test -p slipfork --test status_surface --test ansi_matrix` green locally; `guard.py`.,
