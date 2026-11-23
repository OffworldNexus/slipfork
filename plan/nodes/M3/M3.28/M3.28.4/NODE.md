---
id: M3.28.4
title: "Engine facts: world_processes/world_lineage, per-world BootLineage, injected clock, one origin renderer"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.28.3]
paths:
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-world/src/**"
  - "crates/slipfork-world/tests/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/**"
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/oci_store.rs
  - ARCHITECTURE.md
  - "docs/design/dx-0.1.0-ux.md"
  - "docs/decisions/ADR-0002-*.md"
  - "docs/decisions/ADR-0010-*.md"
  - "docs/design/world-base-image.md"
attempts: 4
max_attempts: 5
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T21:00"
---

# Goal

Fill the engine facts the `0.1.0` wire already carries: `WorldEngine::world_processes` and `WorldEngine::world_lineage` over the engine's own supervision and boot records, a per-world `BootLineage`, rootfs/OCI cache outcome + timings recorded at boot through an injected clock, and the single `WorldOrigin::local` contract (port-less at `:80`, `:<port>` otherwise, port `0` fails closed). Delete the `WorldLineage::unknown()` placeholder that `M3.28.3` installed.

# Context

`M3.28.3` shipped the wire (`IngressInfo`, `WorldSummary.parent`, `WorldProcessReport { state, lineage, services }`, `WorldServiceSummary`, `WorldLineage` + sub-records in `crates/slipfork-engine/src/lineage.rs`) with every engine-owned fact left `None` and one `WorldLineage::unknown()` per call site in `crates/slipfork/src/daemon.rs`. This node is the engine half that makes those fields real. The restart/`up` semantics across edge and daemon are **not** in this node — they are `M3.28.17`.

Scope note from attempt 1: `format!("http://127.0.0.1:<port>")` in `crates/slipfork-snow` is the **in-world loopback** URL for resource seeding, not a `.localhost` origin, so it is out of scope; the one-origin-renderer check is scoped to `WorldOrigin::local` call sites (acceptance below). `crates/slipfork/tests/critical_cli.rs` is Phase C's single-writer file and must not be touched here (attempt 1's edit to it was reverted).

# Design / Invariants

1. `WorldEngine::world_processes(&WorldHandle) -> Vec<WorldServiceInfo>` over the engine's own supervision records (`ServiceProcess`/`ContainerProcess`), deterministic order, `EngineError::WorldGone` for destroyed/foreign handle, `uptime_secs` stamped from the **injected clock** (no `Instant` on the wire, no second clock in the daemon).
2. `WorldEngine::world_lineage(&WorldHandle) -> WorldLineage` filled from **that world's own** records: rootfs digest + cache `hit|miss`, base key + seal millis, seed digest + `rebuilt` + millis, per-service cache/millis. Record the facts at boot where they are not recorded yet (`RootfsBuildEvent::{CacheHit,CacheMiss}` exists; `WorldRootfs`/`OciRootfs` gain `cache` + `millis`; digest from the existing hasher/base-key derivation). Never estimate, never fabricate: unknown → `None`.
3. One injected clock port (`crates/slipfork-engine/src/clock.rs`): `MonotonicClock` for durations, `SystemClock` for wall stamps; wired at the composition root, never read inside a pure function.
4. **Delete `WorldLineage::unknown()`** and every call site (`crates/slipfork/src/daemon.rs`); a world with no recorded facts yields all-`None` fields built from its own records.
5. `WorldOrigin::local(&route, ingress_port)` stays the single `.localhost` origin renderer: `:80` → port-less, `1..=65535` → `:<port>`, port `0` fails closed with a typed error, and a `snow.yml` domain still fails closed. No other module builds a `.localhost` origin.
6. `WorldSpec` carries its `BootLineage`; every `WorldSpec`/`WorldRootfs`/`OciRootfs` literal and `DockerfileStageBuilder` call site is updated in the same sweep (including `crates/slipfork/src/oci_store.rs`, which must record the OCI cache outcome).
7. Keep `StubWorldEngine`/test doubles compiling; no new dependency, no `unsafe`, no global, no `#[allow]`.

# Acceptance

```console
$ cargo test -p slipfork-engine --lib
... test result: ok
$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
... test result: ok
$ cargo test -p slipfork-world --test critical_world
... test result: ok
$ ! grep -rn 'WorldLineage::unknown' crates/ --include=*.rs && echo no-placeholder-lineage
no-placeholder-lineage
$ ! grep -rnE 'format!\("http://\{[a-z_]+(host|route)' crates/ --include=*.rs | grep -v '^crates/slipfork-world/'; echo one-origin-renderer
one-origin-renderer
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
$ cargo fmt --all -- --check && echo fmt-ok
fmt-ok
```

- Engine tests prove a stopped service keeps its row with `pid: None`, two worlds never share rows, `uptime_secs` is monotonic from the injected clock, a second boot of the same base is a cache `hit`, recorded millis are `Some` only for facts the engine measured, and a destroyed handle is `WorldGone`.
- `critical_world` pins port-less-at-80, `:<port>` otherwise, port `0` failing closed, and the existing default-ingress failure unchanged.
- `RESULT.md` records the `git diff --shortstat` and the exact list of call sites updated.
