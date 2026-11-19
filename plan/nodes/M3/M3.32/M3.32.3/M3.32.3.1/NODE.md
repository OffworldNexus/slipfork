---
id: M3.32.3.1
title: "Port-agnostic .localhost ingress: Host port-strip, <label>.* subdomain routes, fail-closed traversal"
kind: task
status: done
owner: implementer
priority: 2
depends_on: []
paths:
  - "crates/slipfork-edge/**"
  - "crates/slipfork-world/src/identity.rs"
  - "crates/slipfork-engine/src/btrfs_engine/crun.rs"
  - "crates/slipfork-engine/src/rootfs/dedupe.rs"
  - "crates/slipfork-world/src/port/values.rs"
  - "crates/slipfork-forkfile/src/check.rs"
  - "docs/design/dx-0.1.0-ux.md"
  - tools/tests/test_do_vm.py
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T19:49"
updated: "2026-11-19T20:02"
---

Goal: strip any port from a .localhost Host before matching (so an `ssh -L` tunnel's local port routes like :80); accept `route mail.*` subdomain entries alongside `/` and `/api`, reusing the canonical `slipfork_world` host/label grammar and keeping the edge free of CLI tables; fail closed on path traversal (including percent-encoded separators) and on hosts outside `.localhost`. The multiplexed `logs -f` fan-in lives in M3.32.3.4. Acceptance: cargo test -p slipfork-edge -p slipfork-wire -p slipfork; clippy -D warnings; fmt; make check. No RESULT.md.

# Acceptance

- `cargo test -p slipfork-edge` exits 0 with the port-strip, subdomain, prefix-precedence and traversal/foreign-host cases.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
