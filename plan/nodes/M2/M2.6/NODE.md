---
id: M2.6
title: "slipfork-engine: multi-mount WorldSpec, redacted ExecEnv, opaque WorldHandle, freeze_world"
kind: task
status: review
owner: implementer
priority: 3
depends_on: [M2.3]
paths: ["crates/slipfork-engine/**", crates/slipfork/src/main.rs]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T19:15"
---

# Goal

Promote `slipfork-engine` to the full M2 `WorldSpec` contract — multi-mount `WorldMount` (read-only/read-write), redacted `ExecEnv`, opaque `WorldHandle`, `ServiceSpec`, `freeze_world` — and make `ReflinkNetnsEngine` implement it with a real CoW lineage that still passes the shared conformance suite.

# Context

- `0.1.0` shipped only the minimal PoC subset of the world spec; `ADR-0008` and `ARCHITECTURE.md` now specify the complete shape.
- `crates/slipfork-engine` gains the `slipfork-world` path dependency in `M2.2` (M1.6 #12) so a world target is a real typed value.
- `unsafe` stays confined to the reflink module; this node must not add any new unsafe code.

# Design / Invariants

1. **Types & `EngineCapabilities` deletion (`ADR-0008` §7):** Delete `0.1.0`'s `EngineCapabilities` boolean bag and `WorldEngine::capabilities()` (`SnapshotMode::StatefulRam` fails closed with `EngineError::StatefulRamUnsupported` on engines lacking RAM snapshots). Define `MountMode { ReadOnly, ReadWrite }`, `WorldMount { host_path, guest_path, mode }`, `WorldSpec { target: WorldTarget, snapshot: SnapshotId, mounts: Vec<WorldMount>, base_env: ExecEnv, local_ingress_port: Option<u16> }`, `SnapshotMode { DiskCow, StatefulRam }`, opaque in-process `WorldHandle` (owned exclusively inside `slipforkd`, `H-010` #1; private fields, stable opaque `Debug`), and `ServiceSpec` (id, argv, cwd, overlay env, readiness port). `ExecEnv` provides a restricted `0600`-IPC wire helper used solely by `DaemonControl::exec_target`.
2. **Env redaction:** the effective env is the base env overlaid by the overlay env (overlay wins); the inherited allowlist is restricted to `PATH`/`TERM`/`LANG` plus a world-scoped `HOME`; a hand-written `Debug` redacts every secret value; the secret is exposed only at the child-process env boundary.
3. **Mounts:** repo mount RW under host `$UID:$GID`, artifact volumes RO, and a validation that fails closed (`EngineError::OverlappingMounts`) for nested RW mounts.
4. **`freeze_world`:** seals an origin-independent live world (`base -> migrate -> seed_*`) into an immutable child snapshot id; freezing an already-frozen or destroyed world fails closed (`EngineError::WorldNotFreezable`), and a clone of a frozen snapshot never sees writes from a sibling clone.
5. **Conformance:** `assert_world_engine_conformance` gains multi-mount, freeze-lineage, opaque-handle, and redacted-`Debug` assertions; `IncusEngine` stays compiled against the same trait (protocol harness only, no live daemon here) so both adapters prove one contract.
6. The crate stays `#![forbid(unsafe_code)]`; env/clock/network access only inside adapters; no engine-name branching in domain code.

# Acceptance

```bash
# 1. Both adapters satisfy the shared conformance suite.
cargo test -p slipfork-engine --all-targets
cargo test -p slipfork-engine --test critical_engine

# 2. Multi-mount and freeze lineage covered by real reflink runs.
cargo test -p slipfork-engine --test critical_engine -- --nocapture mount
cargo test -p slipfork-engine --test critical_engine -- --nocapture freeze

# 3. Handles are opaque and Debug never prints a secret.
cargo test -p slipfork-engine --test critical_engine -- --nocapture redact
! rg -q 'EngineCapabilities' crates/slipfork-engine/src

# 4. Gates.
cargo clippy -p slipfork-engine --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: nested RW mounts refused, freeze-then-destroy double-free refused, and a sibling clone of a frozen snapshot is unaffected by RW mutations (real reflink, no mocks).
