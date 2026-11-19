---
id: M3.32.2
title: "Per-service crun pod engine: shared userns/netns/ipcns, Btrfs volume CoW submounts & direct execve argv"
kind: epic
status: active
priority: 3
depends_on: [M3.32.1]
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork/**"
  - "crates/slipfork-testkit/**"
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T13:15"
---

# Goal

Replace the single-world-`/` `pivot_root` model (`[world] image`) with **one `crun` container per service** inside a shared `slipfork-world-init` namespace set and one shared Btrfs world subvolume, executing every lifecycle step via direct `execve` `argv` — no `/bin/sh -c` anywhere.

# Context

- Current single-rootfs boot: `crates/slipfork-engine/src/world_init.rs`, `crates/slipfork-engine/src/btrfs_engine/`, `crates/slipfork-engine/src/oci.rs`, `crates/slipfork-engine/src/supervise.rs`, `crates/slipfork-engine/src/bin/` (world-init binary). `crates/slipfork-world/src/stream.rs:55-61` documents the `pivot_root` constraint (`chdir` before `exec`) that must become per-service `chdir(<worktree>/<service_dir>)`.
- Rootfs/OCI primitives to reuse: `crates/slipfork-world/src/rootfs.rs:109-144` (`crun_binary_sha256`, `OciRootfs { image_digest, rootfs }`), `crates/slipfork-world/src/lineage.rs:93` (`CrunService` component). Deduplicate builds by `(context, dockerfile, stage)` for `./dir` services and by image digest for `from <registry/repo:tag>`.
- Delete `slipfork.toml` ownership: `crates/slipfork-dag/src/world.rs:37` (`WorldImageConfig`), `crates/slipfork-dag/src/lib.rs:32,199-236` (`SlipforkConfig.world`, `from_toml`). Project identity comes from `Forkfile` (`project <slug>`) or VCS; the DAG `[test]`/`[artifacts]` sections keep their parser but lose the `[world]` table.
- World boot path to rewrite: `crates/slipfork/src/daemon/world.rs` (534 LOC), `crates/slipfork/src/daemon/boot.rs` (392 LOC), `crates/slipfork/src/daemon/backing.rs`, `crates/slipfork/src/daemon/base.rs`; keep boot pipeline cohesive (`<= 500` LOC structs) rather than growing `world.rs`.
- Mount plan: host worktree live via `MS_BIND` owned `$UID:$GID` (inotify HMR intact) **and** each `volume <rel_path>` bind-mounted from `<world_subvol>/volumes/<service>/<sanitized_rel>` over `<worktree>/<service_dir>/<rel_path>`; CoW-cloning `.venv`/`node_modules` on `fork` must stay ~20 ms. Put the pure computation (service id + rel path -> sanitized volume path + override mount order) in a small pure function/struct in `slipfork-world` so it is unit-testable without root.
- Namespaces: every service joins `slipfork-world-init`'s `userns` + `netns` + `ipcns`; services listen on `127.0.0.1:<port>` inside the shared netns so `slipfork-edge` can dial them without host port mapping.
- Direct `execve`: `run` supervises one child; `setup`/`pre-deploy`/`post-deploy`/`failed-deploy`/`task` argv lines run sequentially with `[1/N]` progress and fail fast citing `Forkfile:<line>`. Consume the `EvaluatedFork` types from `crates/slipfork-forkfile` (add the workspace dep here).

# Acceptance

- `cargo test -p slipfork-world -p slipfork-engine -p slipfork-dag` exits `0`, including a pure unit test asserting volume override ordering and sanitization (no root, no Btrfs needed).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit `0`.
- `! grep -rn '/bin/sh -c\|"sh", "-c"' crates/slipfork-engine crates/slipfork-world crates/slipfork/src` exits `0`.
- `! grep -rn 'WorldImageConfig\|slipfork.toml' crates/` exits `0` except inside `slipfork-dag/src/lib.rs` doc comments for the surviving `[test]`/`[artifacts]` sections.
- Existing `crates/slipfork/src/daemon/tests/*` world-boot tests still pass (updated for per-service pods).
- Write NO `RESULT.md`.
