---
id: M3.32.2.2.1.2
title: "slipfork-engine: per-service OCI bundles via CrunSurface + rootfs dedupe"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.2.2.1.1]
paths: ["crates/slipfork-engine/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:26"
updated: "2026-11-19T13:26"
---

# Goal

Materialize one OCI bundle per service under the engine control dir and dedupe rootfs builds.

# Context

Split from `M3.32.2.2.1`. `crates/slipfork-engine/src/btrfs_engine/container.rs:41-72` (`CrunSurface`) is the sole crun argv owner.

- Extend `CrunSurface` to write `ctl/<world_id>/oci/<service>/config.json` with `process.cwd` from `ServicePodSpec` and `linux.namespaces[].path` from `PodNamespaces`.
- New `rootfs/dedupe.rs` (pure) keyed by `(context, dockerfile, stage, build_env)` for `./dir` services and by image digest for `from <image>`.
- Prove bundle JSON + dedupe with one real alpine OCI lab test (no mocks).
- Do NOT yet delete `pivot_root`/`exec_in_world` (that is the next slice).

# Acceptance

- `cargo test -p slipfork-engine` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
