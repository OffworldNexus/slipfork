---
id: M3.32.2.2.1.3.2.1
title: "Init + coordinate flip: drop pivot_root/overlay, publish namespaces.json, host-coordinate bundles, pod-backed legacy verbs"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-testkit/**"
  - crates/slipfork/src/apparmor.rs
  - tools/do_vm.py
  - Makefile
  - Cargo.toml
  - Cargo.lock
attempts: 1
max_attempts: 5
escalation: architect
created: "2026-11-19T13:50"
updated: "2026-11-19T18:42"
---

# Goal

Flip the boot substrate without breaking any journey: no overlay/`pivot_root`, one shared init publishing `namespaces.json`, and bundles in host coordinates.

# Context

Split from `M3.32.2.2.1.3.2`. Keeping the legacy verbs alive (re-implemented over pods) lets the existing six suites stay runnable and VM-verified while the kernel path changes underneath them.

- `world_init.rs` + `src/bin/slipfork-world-init*`: drop overlay/`pivot_root`; publish `namespaces.json` (paths only).
- `world_pod_namespaces` reads it, failing closed with a `fix:` remedy.
- Retire the `/.slipfork/*` guest coordinate scheme for bundles/state/passwd/oci-run (`OCI_BUNDLES_GUEST`, `pod_bundle_guest`, `image_guest_rootfs`) in one change so backing services and pods stay consistent.
- Re-implement `WorldProcess`/`exec_in_world`/`entry_command` as pod-backed calls; do NOT delete them yet.
- Verify on the Btrfs VM (`tools/do_vm.py sync && tools/do_vm.py test`) and record suite counts in `LOG.md`.

# Acceptance

- `cargo test -p slipfork-engine -p slipfork-world -p slipfork-testkit` exits 0; `cargo test --workspace --all-targets --no-run` links.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- `tools/do_vm.py sync && tools/do_vm.py test` exit 0 on `165.232.86.174` with every existing journey still asserting its original property.
- Write NO `RESULT.md`.
