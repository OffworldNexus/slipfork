---
id: M3.32.2.2.1.3.2
title: "slipfork-engine/world init flip: retire pivot_root & WorldProcess, real world_pod_namespaces, migrate protected suites"
kind: epic
status: active
priority: 3
depends_on: [M3.32.2.2.1.3.1]
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-testkit/**"
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:39"
updated: "2026-11-19T13:50"
---

# Goal

Flip the boot model atomically: no overlay/`pivot_root`, one shared init publishing namespace paths, per-service pod exec everywhere, and the protected suites migrated.

# Context

Split from `M3.32.2.2.1.3`. With pod spawn landed, the merged-root verbs can finally go.

- `world_init.rs` + `src/bin/slipfork-world-init*`: drop overlay and `pivot_root`; publish `namespaces.json` (paths only).
- Implement `BtrfsEngine::world_pod_namespaces` for real, failing closed with a `fix:` remedy when the file is absent.
- Redefine bundle coordinates now that there is no world `/` (`OCI_BUNDLES_GUEST`, `pod_bundle_guest`, bind sources) so backing services and pods stay consistent in one commit.
- Delete `exec_in_world`, `entry_command`, `ServiceRuntime::WorldProcess`; replace with `exec_in_service`/`service_entry_command` over `crun exec`.
- Migrate every affected protected journey in `crates/slipfork-engine/tests/critical_*.rs`, `crates/slipfork-world`, `support/mod.rs`, and `slipfork-testkit/src/conformance.rs`. This node is explicitly authorized to edit them; never weaken an assertion, skip a journey, or delete one.
- Verify live behaviour on the Btrfs lab VM (`tools/do_vm.py sync && tools/do_vm.py test`) because these journeys cannot run on the XFS dev host; record the evidence in `LOG.md`.

# Acceptance

- `cargo test -p slipfork-engine -p slipfork-world -p slipfork-testkit` exits 0 (unit/pure), `cargo test --workspace --all-targets --no-run` links.
- No `pivot_root`, `exec_in_world`, `entry_command`, or `WorldProcess` remains under `crates/`.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test` exit 0 on `165.232.86.174`.
- Write NO `RESULT.md`.
