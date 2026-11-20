---
id: M3.32.2.2.1.3.2.2.2
title: Delete WorldProcess/exec_in_world/entry_command once the daemon has cut over
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.2.2.1.3.2.2.1, M3.32.2.2.2, M3.32.2.2.3]
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-wire/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork/**"
  - "crates/slipfork-edge/**"
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T18:43"
updated: "2026-11-19T22:57"
---

# Goal

Final scorched-earth deletion of the merged-root vocabulary after the daemon cut-over lands.

# Context

Split from `M3.32.2.2.1.3.2.2`. Runs only after `M3.32.2.2.3` makes the daemon spawn `ServicePod`s.

- Delete `WorldProcess`, `exec_in_world`, `entry_command`, `run_in_world`, `entry_prefix`, `WorldProcessBundle`, `SpawnPlan::World`, `WorldExecTarget` bridges, and any wire doc mentioning them.
- Update `slipfork-wire` control docs and every remaining caller.

# Acceptance

- No `WorldProcess`, `exec_in_world`, `entry_command`, `run_in_world`, `entry_prefix` under `crates/`.
- Local suites green; `tools/do_vm.py sync && tools/do_vm.py test` exit 0.
- Write NO `RESULT.md`.
