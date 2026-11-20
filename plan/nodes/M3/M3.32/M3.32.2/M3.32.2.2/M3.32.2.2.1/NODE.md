---
id: M3.32.2.2.1
title: "slipfork-world pod port surface + slipfork-engine shared-ns init & per-service CrunSurface bundles"
kind: epic
status: done
priority: 3
depends_on: []
paths: ["crates/slipfork-world/**", "crates/slipfork-engine/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:24"
updated: "2026-11-19T22:58"
---

# Goal

Give the per-service pod spec an injection path through the `WorldEngine` port and boot it as one `crun` container per service on a shared namespace set.

# Context

Split from `M3.32.2.2`. Upstream: `crates/slipfork-world/src/pod.rs` (`ServicePodSpec`, `PodLocation`, `PodNamespaces`, `VolumePlan`, `PodCommand`, `mounts()`) and `crates/slipfork-engine/src/btrfs_engine/container.rs:41-72` (`CrunSurface`, the single argv owner).

- Extend the port in `crates/slipfork-world/src/port/values.rs:242-264` so `WorldEngine` can receive per-service pod specs (e.g. a pod runtime variant / `spawn_pod(&ServicePodSpec)`); keep `ServiceRuntime` cohesive, no mode flags, no downcast.
- Rewrite `crates/slipfork-engine/src/world_init.rs` + `src/bin/slipfork-world-init*`: drop overlay/`pivot_root`, keep one shared init that exposes its `user`/`net`/`ipc` ns paths to the engine.
- Emit one OCI bundle per service under the engine control dir (`ctl/<world_id>/oci/<service>/`), with `process.cwd` from `ServicePodSpec` and `linux.namespaces[].path` from `PodNamespaces`; dedupe rootfs by `(context, dockerfile, stage, build_env)` or image digest.
- Do not build crun argv outside `CrunSurface`.

# Acceptance

- `cargo test -p slipfork-world -p slipfork-engine` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- No `/proc/<pid>/ns` literal outside test fixtures; no `pivot_root` left in engine boot.
- Write NO `RESULT.md`.
