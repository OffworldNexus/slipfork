---
id: M3.2
title: "Polyglot Parallel Snapshot-DAG Test Orchestrator (slipfork test, RunnerProtocol v1, warm world pool & CoverageDriver)"
kind: epic
status: ready
priority: 2
depends_on: [M3.1]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T06:34"
---

# Goal

Deliver Slipfork `0.3.0` (`slipfork test`):
1. **Snapshot & Cache Lifecycle (`slipfork-engine`, `slipfork-dag`, `slipforkd`):** Content-addressed `[artifacts.*]` (`CowArtifactStore` + `VolumeCloner`) and `[fixtures.*]` materialization (origin-independent shared `DigestCache` CoW lineages sealed via `freeze_world` vs `per_world = true` post-origin world seeds that never enter `DigestCache`), plus snapshot and `<state_dir>/cache/{rootfs,oci}` garbage collection in `slipfork prune`.
2. **`crates/slipfork-test` (`TestCoordinator`, `RunnerProtocol v1`, Warm Pool & `CoverageDriver`):** Wire `TestCoordinator` into `slipforkd` over canonical `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` on `ADR-0009` `Dockerfile` `L0` rootfs + `crun` backing-service worlds; enforce hermetic-by-default `ResourceMode::Test` (`ResourcePlan::resolve`: ephemeral local SeaweedFS S3 + clean `initdb -> migrate -> [fixtures.*]`); rolling pre-warmed `TestSlot` CoW world pool (with slot-stable coverage mounts) + shared host `BrowserContext`s over `http://test-<slot>.<project>.localhost`; two-tier `batch` vs `world` scheduling; universal `AF_UNIX` NDJSON `RunnerProtocol v1` (`SocketRunnerAdapter` + zero-plugin `CommandTestAdapter`); built-in typed `[test]` coverage (`coverage`, `coverage_report`, `coverage_fail_under` `0..=100`) via `TestAdapter::coverage` (`CoverageDriver`); and surviving tool-owned `SLIPFORK_OUTPUT_DIR`.
3. **Polyglot `TestAdapter`s & `ADR-0007` Tier Promotion:** Ship and verify `pytest-slipfork` (Adapter #1 + warm Zygote), `Playwright`/`Vitest`, `PHPUnit`/`Pest`, `cargo nextest`, and `CommandTestAdapter` with protected critical suites, published Zensical recipes, and `support/integrations.toml` tier promotion (`tools/check_support.py --check`).

# Acceptance

- [ ] `slipfork test` executes real multi-slot parallel suites over `RunnerProtocol v1` (`SocketRunnerAdapter` + `CommandTestAdapter`) on `ADR-0009` `L0` rootfs + `crun` worlds with hermetic `ResourceMode::Test` defaults, orthogonal `[artifacts.*]` / `[fixtures.*]` (`per_world` non-sharing enforced), typed `[test]` coverage reports/thresholds, and surviving `SLIPFORK_OUTPUT_DIR`.
- [ ] `pytest-slipfork`, `Playwright`/`Vitest`, `PHPUnit`/`Pest`, `cargo nextest`, and `CommandTestAdapter` pass shared `TestAdapter` conformance + protected critical tests and match `docs/support-matrix.md` (`check_support.py --check`).
- [ ] `make check` (without env overrides) and bounded `make bench --check` exit 0 with zero published vaporware (`check_docs_honesty.py`).
