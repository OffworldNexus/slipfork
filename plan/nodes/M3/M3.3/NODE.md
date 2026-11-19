---
id: M3.3
title: "Isolated: live IncusEngine conformance and declared support promotion (gated on H-004)"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.1]
paths:
  - "crates/slipfork-engine/src/incus.rs"
  - "crates/slipfork-engine/tests/critical_engine.rs"
  - "crates/slipfork/src/**"
  - support/integrations.toml
  - "docs/support-matrix.md"
  - "docs/reference/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T16:53"
---

# Goal

Prove the live `IncusEngine` against a real project-scoped daemon and promote the incus integration in the support registry — **only** if the missing host input (`H-004`) is available. Carried forward from `M2.21` post-`0.2.0`; when `H-004` remains open, this node performs zero file edits and stays `blocked`.

# Context

- `IncusEngine` compiles against `WorldEngine`, imports `WorldSpec.rootfs.app_rootfs` once per `rootfs.digest` as `slipfork-l0-<digest12>` (`ADR-0009` §3), and passes protocol-harness conformance; live daemon conformance awaits open `H-004` (project-scoped `incusd` socket).
- Terminal and isolated: nothing in `M3` depends on this node.

# Design / Invariants

1. Probe the injected/documented project-scoped `incusd` socket paths read-only first; if absent, perform zero edits and stay `blocked` (`missing-input`, `H-004`).
2. When `H-004` is provided: materialize a real `WorldRootfs` via `BaseImageBuilder`, pass the full `assert_world_engine_conformance` matrix (`WorldProcess` + `OciContainer` supervision, `freeze_world` stop ordering, `exec_in_service`, `service_entry_command`, `dial_world_tcp`), wire CLI selection, and promote `incus-engine` to `declared` via `make support-sync`.

# Acceptance

```bash
uv run --no-project python -c 'import os; c=["%s/incus/unix.socket" % os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid()), "/run/incus/unix.socket", "/var/lib/incus/unix.socket"]; hit=[p for p in c if os.path.exists(p)]; print("H-004: live incusd socket at " + hit[0]) if hit else print("H-004: absent -> zero edits, STATUS: blocked (missing-input)")'
```
