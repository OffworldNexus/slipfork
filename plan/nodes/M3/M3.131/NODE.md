---
id: M3.131
title: "Live tests can silently use a stale installed slipfork-world-init (VM /usr/bin), hiding real failures"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.120]
paths:
  - tools/do_vm.py
  - TESTING_PLAN.md
  - Makefile
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/init.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T21:28"
updated: "2026-11-25T11:20"
---

# Problem

During `M3.130` the live `critical_status_live` replay assertion failed with a `┃` gutter mismatch that turned out to be a **stale `/usr/bin/slipfork-world-init`** left from an older build on the VM, not a product bug. `WorldEnv::init_binary` / `HostEnv` resolves the init from PATH (and the lab runs on a VM whose `/usr/bin` copy predates the checkout), so a live suite can exercise yesterday's init and either fail for the wrong reason or pass while the shipped init is broken.

# Fix

- One owner for init resolution: the lab harness passes the **freshly built** init (workspace `target/.../slipfork-world-init` or an explicit `SLIPFORK_WORLD_INIT`, which `do_vm.py` sets after building it alongside `slipfork`) and never falls back to a PATH copy that differs from the checkout.
- Fail closed when the resolved init is older than the binary under test (same build-stamp idea as `M3.128`): one line naming both paths, `next: cargo build -p slipfork-world --bin slipfork-world-init` (or the `do_vm.py` stage).
- Document the rule in `TESTING_PLAN.md` (one short paragraph): a live run never tests an installed binary the checkout did not build.

# Acceptance

- On the VM, with a deliberately stale `/usr/bin/slipfork-world-init`, a live suite either passes using the fresh init or fails closed with the one-line remedy — never silently uses the stale one.
- `make check`; a live suite on the VM private tree (never `do_vm.py sync`, never `~/demo`) proves both cases.
- Commit `[M3.131] ...`, done, no push.
