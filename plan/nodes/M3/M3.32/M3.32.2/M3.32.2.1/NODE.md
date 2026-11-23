---
id: M3.32.2.1
title: "slipfork-world pod primitives (VolumePlan/ServicePodSpec + crun argv) & dag [world] excision"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork-dag/**"
  - "crates/slipfork-vcs/src/ports.rs"
  - crates/slipfork/src/daemon/resolve.rs
  - crates/slipfork/src/daemon/types.rs
  - "crates/slipfork-engine/src/rootfs/**"
  - "crates/slipfork-engine/tests/critical_rootfs_contract.rs"
  - docs/reference/manifest.md
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T13:15"
updated: "2026-11-19T13:23"
---

# Goal

Add pure per-service pod primitives to `slipfork-world` and delete the `slipfork.toml` `[world]` ownership from `slipfork-dag`.

# Context

Split from `M3.32.2` (see `plan/nodes/M3/M3.32/M3.32.2/NODE.md` and `docs/design/forkfile.md` section 3).

- New `crates/slipfork-world/src/pod.rs`: pure `VolumePlan` + `ServicePodSpec` - sanitize `<service>/<rel_path>` into `<world_subvol>/volumes/<service>/<sanitized_rel>`, compute deterministic override mount order (live `MS_BIND` worktree first, volume submounts after), and build the shared-namespace `crun` argv (`--join` userns/netns/ipcns, `chdir` = `<worktree>/<service_dir>`). Rootless unit tests only.
- Delete `crates/slipfork-dag/src/world.rs`, `SlipforkConfig.world`, and the `from_toml` `[world]` branch; keep `[test]`/`[artifacts]`/`[resources]` parsing. Remove `WorldImageConfig` consumers including `crates/slipfork-vcs/src/ports.rs`.
- Do not delete `slipfork-snow` (removed in `M3.32.3`).

# Acceptance

- `cargo test -p slipfork-world -p slipfork-dag` exits 0, including pure volume-sanitization/override-order unit tests (no root, no Btrfs).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- `! grep -rn WorldImageConfig crates/` exits 0.
- Write NO `RESULT.md`.
