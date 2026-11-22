---
id: M3.50.6
title: "Rebuild .deb + live 165.232.86.174 proof: real-PTY shell, <1s 8-component fork"
kind: task
status: active
owner: implementer
priority: 5
depends_on: [M3.50.3, M3.50.4, M3.50.5, M3.50.7, M3.50.8, M3.50.9, M3.50.10]
paths:
  - "scripts/**"
  - Makefile
  - tools/do_vm.py
  - "support/**"
  - "crates/slipfork-world/src/engine.rs"
  - "crates/slipfork-world/src/engine/**"
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/fork_nonbtrfs.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - "crates/slipfork-world/tests/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T15:58"
---

# Goal
Fresh distro packages install and prove on `165.232.86.174`: real-PTY `slipfork shell api|app_db|cache`, no `db`/`redis` verbs, `DECSTBM` bottom bar with one line per task, and `time slipfork fork probe-speed` on the full 8-component `/home/slipfork/demo` under `1.0s`, followed by `slipfork drop probe-speed` and cleanup of every leftover fork (`tralala`, probe forks).

# Context
- Host: `ssh -i /home/remy/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174`; `slipfork` runs as user `slipfork` — `su - slipfork -c '<cmd>'`.
- Build/install: `make check`, `make docs-check`, `make packages`, `make packages-test` (`Makefile:183-199`, `scripts/build-deb`, `scripts/check-portable-elf.sh`).
- `tools/do_vm.py:114` still expects `support/demo` (restored by M3.50.3); verify `tools/do_vm.py sync`/`test` resolve it. Only this file may be edited here.
- Live suites, sequentially on real Btrfs + `crun`: `critical_btrfs_live`, `critical_oci_live`, `critical_dockerfile_live`, `critical_pod_runtime`, `critical_world_journey`, `critical_fork_live`, `critical_status_live`.
- Demo: `/home/slipfork/demo` (9 files, rows `pg`, `cache`, `uploads`, `assets`, `api`, `worker`, `front`, `mailpit`, `arg VITE_CDN_URL = static-cdn.url`); refresh `feat-auth`; the frontend digest must not churn per fork.
- Pitfalls: `/tmp` is tmpfs — snapshots/volumes under `~/.cache/slipfork`; never leave `probe-speed`/`tralala` behind; `slipfork shell` proof needs a real PTY (ssh `-tt` or `script -qec`) showing `tty` -> `/dev/pts/*`, zero `can't access tty`, prompt `(demo:api) ~/app$`; `slipfork db`/`slipfork redis` exit `2` unknown subcommand.

# Acceptance
- `make check`, `make docs-check`, `make packages`, `make packages-test`, `uv run --no-project tools/plan.py validate`, `uv run --no-project tools/guard.py` all exit `0`.
- On `165.232.86.174` (paste the transcript in the summary, no `RESULT.md`): all 7 live suites green; real-TTY `shell api|app_db|cache`; `fork probe-speed` total wall-clock `< 1.0s` across 8 rows with one line per task and `cached` labels; `slipfork drop probe-speed`; `slipfork ps` lists only `demo` and `feat-auth`.
