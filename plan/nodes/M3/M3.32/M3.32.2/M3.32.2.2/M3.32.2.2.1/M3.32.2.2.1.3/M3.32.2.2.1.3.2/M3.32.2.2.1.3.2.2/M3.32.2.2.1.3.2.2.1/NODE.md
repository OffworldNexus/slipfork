---
id: M3.32.2.2.1.3.2.2.1
title: Component ServicePod path in engine + journey rebase off WorldProcess
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.2.2.3]
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-testkit/**"
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-19T18:43"
updated: "2026-11-19T22:49"
---

# Goal

Make world components explicit service pods and move every engine journey onto `ServicePod` + `exec_in_service`, while the legacy verbs remain as a shrinking bridge for the daemon.

# Context

Split from `M3.32.2.2.1.3.2.2`. `crates/slipfork/src/daemon/world.rs:319` still constructs `ServiceRuntime::WorldProcess`, and the daemon/wire call `exec_in_world`/`entry_command`, so the trait removal must wait for `M3.32.2.2.3`.

- Engine: spawn components as `ServiceRuntime::ServicePod` (bundle dir, `VolumePlan`, world mounts, cwd, readiness).
- Rebase `critical_{engine,btrfs_live,oci_runtime,pod_runtime}.rs`, `support/mod.rs`, `slipfork-testkit/src/conformance.rs`, and `crates/slipfork-world` tests onto `ServicePod` + `exec_in_service`/`service_entry_command`.
- Keep the legacy methods + `SpawnPlan::World` as a bridge (no new users).
- Add the `write_pod_bundle` doc line: "bundle synthesis only — for a live world use `pod_bundle_dir` + `world_mount_path`".

# Acceptance

- Local: `cargo test -p slipfork-engine -p slipfork-world -p slipfork-testkit`, fmt, clippy `-D warnings`, `--all-targets --no-run`.
- VM: `tools/do_vm.py sync && tools/do_vm.py test` exit 0 (12 suites).
- No protected assertion weakened.
- Write NO `RESULT.md`.
