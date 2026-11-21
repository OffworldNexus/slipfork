---
id: M3.40.12
title: Live Btrfs lab green (crun exec/forks/status) and repo hygiene for the M3.40 gate
kind: task
status: done
owner: expert
priority: 3
depends_on: [M3.40.11]
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork/**"
  - .gitignore
  - editors/zed/check.sh
  - Makefile
  - "specs/**"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T14:58"
updated: "2026-11-21T15:20"
---

# Goal
Make every protected live Btrfs suite green on the lab droplet and leave the repository hygiene checks clean for the `M3.40` gate.

# Context
- Local container has no Btrfs kernel support; the lab is the existing droplet (`tools/do_vm.py ssh`), tree `/home/slipfork/slipfork`, Btrfs at `/var/lib/slipfork`, tmpfs runtime `/dev/shm/slipfork-live-runtime`.
- First observed failure: `crates/slipfork-world/tests/critical_pod_runtime.rs:749` `exec must forward the child's exit code` — `crun exec` returned 255 (runtime error) instead of the child's 3.
- `editors/zed/check.sh` builds into `editors/zed/target`, which `tools/guard.py` rejects as an untracked artefact.

# Acceptance
- `cargo test -p slipfork-world --features slipfork-world/live --test critical_pod_runtime -- --test-threads=1` green on the lab.
- `cargo test -p slipfork --features slipfork/live --test critical_world_journey -- --test-threads=1`, `--test critical_fork_live`, `--test critical_status_live` green on the lab.
- `editors/zed/check.sh` builds into the root `target/` (ignored) and `tools/guard.py` is clean after `make check`.
- `cargo fmt --all -- --check`; `cargo clippy -p slipfork-world -p slipfork --all-targets -- -D warnings`; `critical_guard.py --base 74f097a --node M3.40`; `guard.py`.
