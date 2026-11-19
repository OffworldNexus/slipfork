---
id: M3.32.2.2.1.3.1
title: "slipfork-engine service-pod spawn & supervision (crun run, retained logs, readiness, stop/teardown)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-testkit/src/conformance.rs"
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T13:39"
updated: "2026-11-19T13:49"
---

# Goal

Make `ServiceRuntime::ServicePod` actually spawn: bundle on disk, `crun run`, retained logs, readiness, stop and teardown — while the current world init still exists.

# Context

Split from `M3.32.2.2.1.3`. `CrunSurface::write_pod_bundle` already emits a correct guest-coordinate bundle; `start_service` still returns `Unsupported{capability:"service_pod"}`.

- Replace that refusal with a real pod spawn path: materialize the bundle, launch `crun run` joined to the injected `PodNamespaces`, capture stdout/stderr into the existing retained log surface, and record the service row like a backing container.
- Readiness, stop (process group / `-pgid` safe), and teardown must cover pod bundles and their rootfs bind mounts (today teardown covers backing services only).
- Keep the existing init and `WorldProcess` verbs alive; no port deletions in this slice.
- Prove one pod `crun exec` journey in a protected lab suite; it runs on Btrfs only, so gate with the existing `require_btrfs` helper and state that explicitly.

# Acceptance

- `cargo test -p slipfork-engine` exits 0 (pure/unit suites), `cargo test --workspace --all-targets --no-run` links.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- The new pod journey is registered in the Makefile suite list if it is a new binary.
- Write NO `RESULT.md`.
