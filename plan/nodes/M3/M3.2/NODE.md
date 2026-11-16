---
id: M3.2
title: "Polyglot Parallel Snapshot-DAG Test Orchestrator (slipfork test, RunnerProtocol v1, warm world pool & CoverageDriver)"
kind: epic
status: ready
priority: 2
depends_on: [M3.1, M3.19]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T16:59"
---

# Goal

Deliver Slipfork `0.3.0` (`slipfork test`) on the **single** Btrfs world engine — one unified Btrfs subvolume per world behind `BtrfsEngine` (`ADR-0010`) — with zero fallbacks:

1. **Snapshot & cache lifecycle:** content-addressed `[artifacts.*]` (`CowArtifactStore` + `VolumeCloner`) and `[fixtures.*]` materialization (origin-independent `DigestCache` lineages sealed via `freeze_world` vs `per_world = true` post-origin seeds that never enter `DigestCache`), plus unreferenced subvolume + `<state_dir>/cache/{rootfs,oci}` garbage collection in `slipfork prune`.
2. **`crates/slipfork-test` (`TestCoordinator`, `RunnerProtocol v1`, warm pool, `CoverageDriver`):** wired into `slipforkd` over the canonical `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` tree; hermetic-by-default `ResourceMode::Test` via `ResourcePlan::resolve`; rolling single-use pre-warmed `TestSlot` worlds with slot-stable coverage mounts; two-tier `batch` vs `world` scheduling; `AF_UNIX` NDJSON `RunnerProtocol v1` (`SocketRunnerAdapter` + zero-plugin `CommandTestAdapter`); built-in typed `[test]` coverage (`coverage`, `coverage_report`, `coverage_fail_under`) through `TestAdapter::coverage`; surviving tool-owned `SLIPFORK_OUTPUT_DIR`.
3. **Adapters & evidence:** `pytest-slipfork` (Adapter #1, warm Zygote `os.fork()` after app import) and `CommandTestAdapter` pass the shared `TestAdapter` conformance suite and protected critical tests; registry claims follow `ADR-0007` evidence only (`tools/check_support.py --check`). (`WeavedRegistrar`, externally gated on `H-006`, was moved from `M3.4` to `M4.1`.)

# Acceptance

- [ ] `slipfork test -j N` executes real multi-slot parallel suites over `RunnerProtocol v1` on Btrfs `crun` worlds with hermetic `ResourceMode::Test` defaults, orthogonal `[artifacts.*]`/`[fixtures.*]` (`per_world` non-sharing enforced), typed coverage reports/thresholds, and surviving `SLIPFORK_OUTPUT_DIR`.
- [ ] `pytest-slipfork` and `CommandTestAdapter` pass the shared `TestAdapter` conformance suite plus protected critical tests and match `docs/support-matrix.md` (`check_support.py --check`).
- [ ] `make check` (zero environment overrides) and `make docs-check` exit 0 on a non-Btrfs dev host; the real kernel suites and a bounded `make bench --check` exit 0 on the Btrfs DO VM with zero published vaporware (`check_docs_honesty.py`).
