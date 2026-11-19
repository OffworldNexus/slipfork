---
id: M3.32.2.2
title: "slipfork-engine per-service crun pod runtime, shared ns init & direct execve lifecycle"
kind: epic
status: active
priority: 3
depends_on: [M3.32.2.1]
paths: ["crates/slipfork-engine/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:15"
updated: "2026-11-19T13:24"
---

# Goal

Run one `crun` container per service inside a shared `slipfork-world-init` userns/netns/ipcns and execute every lifecycle step through direct `execve` argv.

# Context

See `plan/nodes/M3/M3.32/M3.32.2/NODE.md` lines 29-35 and `docs/design/forkfile.md` section 3.

- Replace the `pivot_root` model in `crates/slipfork-engine/src/world_init.rs` and `src/bin/slipfork-world-init*` with a shared namespace init that services join.
- New `btrfs_engine/pod.rs` (or equivalent cohesive module <= 500 LOC) creating one `crun` per service on the shared ns set with `chdir(<worktree>/<service_dir>)`.
- Consume `slipfork-forkfile::EvaluatedFork` (add workspace dep); dedupe rootfs builds by `(./dir, dockerfile, stage, build_env)` and by image digest for `from <image>`.
- `run` supervises one child; `setup`/`pre-deploy`/`post-deploy`/`failed-deploy`/`task` argv lines run sequentially with `[1/N]` progress and fail fast citing `Forkfile:<line>`; zero `/bin/sh -c` (also drop it from `stream.rs`).

# Acceptance

- `cargo test -p slipfork-engine -p slipfork-world` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- Zero `/bin/sh -c` or `sh -c` argv construction under `crates/slipfork-engine`/`crates/slipfork-world` (`grep` must find nothing).
- Write NO `RESULT.md`.
