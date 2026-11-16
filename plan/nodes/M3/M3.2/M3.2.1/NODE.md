---
id: M3.2.1
title: "0.3.0 test-orchestrator architecture blueprint: TestAdapter/CoverageDriver DI tree, RunnerProtocol v1, WorldPool, daemon seam"
kind: task
status: ready
owner: architect
priority: 1
depends_on: []
paths: [ARCHITECTURE.md, "docs/design/m3-test-orchestrator.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Lock the `0.3.0` test-orchestrator architecture **before** any `crates/slipfork-test` code lands: the `TestAdapter`/`CoverageDriver` capability tree, `RunnerProtocol v1` frames and error taxonomy, the rolling single-use `WorldPool` lifecycle on `BtrfsEngine`, the fixture/artifact ownership split, and the single daemon seam through which `TestCoordinator` reaches worlds.

# Context

- `ARCHITECTURE.md` §3.6 already sketches `TestAdapter`, `CoverageDriver`, `WorldSlotContext`, and `DiscoveredTest`, but leaves open: how `TestCoordinator` (in `crates/slipfork-test`) reaches a `WorldEngine` that lives inside `slipforkd`; how the warm pool is refilled/drained; who owns `CowArtifactStore`/`VolumeCloner`; and how protocol errors are typed.
- `ADR-0006` §§3–5 and `ADR-0003` §1 are the normative protocol/coverage/fixture sources; `ADR-0010` (Btrfs-only) is the storage substrate. Every Phase 2 leaf below depends on this blueprint, so lock it here once instead of discovering it in five parallel leaves.

# Design / Invariants

1. **Composition & seam:** `TestCoordinator` is constructed inside `slipforkd` (the sole Composition Root) and never owns an engine directly; it consumes an injected narrow capability trait for worlds/slots (e.g. `TestWorldFactory`/slot leasing) that `slipforkd` implements over `dyn WorldEngine` + `ResourcePlan::resolve`. The CLI reaches it through exactly one new typed `DaemonControl::test` request streaming typed `TestEvent`s (no second socket, no `SCM_RIGHTS`, no CLI-side coordinator).
2. **`TestAdapter` capability tree:** `discover` → `Vec<DiscoveredTest { id, fixture, isolation: Batch | DedicatedWorld }>`; `run_items` streams `TestOutcome`; `coverage() -> Option<&dyn CoverageDriver>`. No boolean feature bags, no downcasts, no `match adapter_name`.
3. **`RunnerProtocol v1`:** `AF_UNIX` NDJSON at `.slipfork/runs/<run_id>/<slot>/runner.sock`, versioned frames (`discover`/`discovered`, `run`/`item_result`/`run_complete`), bounded frame size, explicit typed error frame, and a fail-closed rule for unknown `version`, unknown `op`, and item ids the adapter never discovered.
4. **`WorldPool`:** `W` pre-booted pristine worlds (`test-<slot>.<project>.localhost`), **single-use** per slot, background refill threads, bounded timeouts that drain and destroy a stuck slot, and slot-stable `.slipfork/runs/<run_id>/<slot>/{coverage,output}` mounted at pre-warm boot. Pool retirement must reuse `BtrfsEngine::destroy_world` (one subvolume, no leaked state).
5. **Fixture/artifact ownership:** `[artifacts.*]` via `CowArtifactStore` + `VolumeCloner`; `[fixtures.*]` origin-independent lineages sealed via `freeze_world` into `DigestCache`, `per_world = true` steps run **post-origin** and are never inserted into `DigestCache` (`ADR-0003` §1). `ResourcePlan::resolve(ResourceMode::Test)` stays the sole hermeticity decision point.
6. **Zero shell interpolation:** every path/env value is typed and adapter-owned; no `sh -c`, no stringified paths, no user-supplied shells in coverage collection.
7. `ARCHITECTURE.md` §2/§3.6 must reflect the final trait signatures exactly; `docs/design/m3-test-orchestrator.md` carries the diagram and the leaf-by-leaf sequencing table matching `M3.2.2`–`M3.2.18`.

# Acceptance

```bash
test -f docs/design/m3-test-orchestrator.md
grep -n 'DaemonControl::test\|TestEvent' docs/design/m3-test-orchestrator.md
grep -n 'WorldPool\|single-use\|refill' docs/design/m3-test-orchestrator.md
grep -n 'runner.sock' docs/design/m3-test-orchestrator.md
grep -n 'per_world' docs/design/m3-test-orchestrator.md
grep -n 'trait TestAdapter' ARCHITECTURE.md
grep -n 'TestWorld' ARCHITECTURE.md
grep -n 'M3.2.2' docs/design/m3-test-orchestrator.md
make docs-check
uv run --no-project tools/plan.py validate
```

Expected: the blueprint exists and names the single daemon seam, the pool lifecycle, the canonical run tree, and the `per_world` non-sharing rule; `ARCHITECTURE.md` carries the locked trait/ seam names; the sequencing table references the real leaf ids; `make docs-check` and `plan.py validate` exit 0.
