---
id: M3.32.2.2.1.1
title: "slipfork-world: additive ServicePod runtime + world_pod_namespaces port values"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork-engine/src/btrfs_engine/**"
  - "crates/slipfork-testkit/src/conformance.rs"
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T13:26"
updated: "2026-11-19T13:30"
---

# Goal

Add the pod-facing port surface additively, with no behaviour change yet.

# Context

Split from `M3.32.2.2.1`. Today `ServiceRuntime` (`port/values.rs:242-264`) has only `WorldProcess`/`OciContainer`, so `pod::ServicePodSpec` cannot reach the engine.

- Add `ServiceRuntime::ServicePod(ServicePodSpec)` and a `WorldEngine::world_pod_namespaces(&WorldHandle) -> PodNamespaces` value method.
- Keep it pure: no new trait objects, no mode flags, no downcast, no consumers required yet.
- `PodNamespaces`, `ServicePodSpec`, `PodLocation`, `VolumePlan` already exist in `pod.rs`; reuse them, do not duplicate.

# Acceptance

- `cargo test -p slipfork-world` exits 0, including a unit test that a pod runtime round-trips its `ServicePodSpec` through the port value.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
