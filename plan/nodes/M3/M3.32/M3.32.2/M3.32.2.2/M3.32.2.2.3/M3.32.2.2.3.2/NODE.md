---
id: M3.32.2.2.3.2
title: "PodBoot/ServicePod daemon boot on EvaluatedFork with per-service exec targets"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.2.2.3.1, M3.32.3]
paths: ["crates/slipfork/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T19:03"
updated: "2026-11-19T19:03"
---

# Goal

Boot the daemon from `Forkfile` with one pod per service and per-service exec targets.

# Context

Split from `M3.32.2.2.3`; runs after the `M3.32.3` Forkfile cut-over so the daemon already has `EvaluatedFork`.

- Rewrite `daemon/{world,boot,backing,base}.rs` into cohesive `PodBoot`/`ServicePod` structs (<= 500 LOC each), wired only from `main.rs`.
- Every component plus pg/redis becomes a service pod; per-service `volume` submounts over the live `MS_BIND` worktree (inotify HMR intact); CoW clone on `fork` stays ~20 ms.
- `exec`/`shell`/`db`/`redis`/`logs` address a named service; `fork` clone untouched.
- Migrate `crates/slipfork/src/daemon/tests/*` and pivot-root-keyed journeys; never weaken or delete one.

# Acceptance

- `cargo test -p slipfork` exits 0; clippy/fmt clean.
- No `sh -eu -c` or `slipfork.toml` under `crates/`.
- VM `tools/do_vm.py sync && tools/do_vm.py test` exit 0 (12 suites).
- Write NO `RESULT.md`.
