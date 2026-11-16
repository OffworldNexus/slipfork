---
id: M3
title: "Polyglot Parallel Snapshot-DAG Test Orchestrator (0.3.0)"
kind: milestone
status: ready
priority: 3
depends_on: [M2]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-14T22:03"
---

# Goal

Deliver Slipfork `0.3.0` (`slipfork test`) atop `0.2.0`'s `slipforkd` Composition Root, `ADR-0009` `Dockerfile` `L0` rootfs + `crun` service containers, and `freeze_world` CoW base snapshots:
1. **Deterministic `make check` & Cache GC (`M3.1` + `M3.2`):** Serialize world-booting Rust critical suites in `Makefile` `rs-test` so plain `make check` exits 0 deterministically under shared-host load without `RUST_TEST_THREADS=1`, and ship snapshot + `<state_dir>/cache/{rootfs,oci}` GC in `slipfork prune`.
2. **Orthogonal Artifact/Fixture Materialization & `crates/slipfork-test` (`M3.2`):** Materialize `[artifacts.*]` (`CowArtifactStore` + `VolumeCloner`) and `[fixtures.*]` (origin-independent shared `DigestCache` CoW lineages vs `per_world = true` post-origin world seeds), wire `TestCoordinator` over `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` with hermetic-by-default `ResourceMode::Test`, rolling pre-warmed `TestSlot` CoW world pool + shared host `BrowserContext`s over `http://test-<slot>.<project>.localhost`, `RunnerProtocol v1` (`SocketRunnerAdapter` + `CommandTestAdapter`), built-in typed `[test]` `CoverageDriver`, and surviving `SLIPFORK_OUTPUT_DIR`.
3. **Tier-Verified Polyglot `TestAdapter`s & Isolated External Gates (`M3.2`, `M3.3`, `M3.4`):** Ship `pytest-slipfork` (#1 + warm Zygote), `Playwright`/`Vitest`, `PHPUnit`/`Pest`, `cargo nextest`, and `CommandTestAdapter` verified by `tools/check_support.py --check`, while keeping live `IncusEngine` (`M3.3`, `H-004`) and `WeavedRegistrar` (`M3.4`, `H-006`) isolated.

# Acceptance

- [ ] Plain `make check` (zero environment overrides) and bounded `make bench --check` exit 0, and every `M3` exit criterion in `plan/ROADMAP.md` is verified.
