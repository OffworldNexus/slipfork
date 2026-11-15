# ADR-0006: World-Owns-The-Stack Inversion, Polyglot `RunnerProtocol v1`, and Built-In Typed Coverage

- **Status:** Accepted — amended at `0.1.0` release gate (normative specification for the "no stringly plumbing" manifest law, world-owned-stack testing, `RunnerProtocol v1`, built-in typed coverage, and tool-owned `SLIPFORK_OUTPUT_DIR` orthogonality)
- **Date:** 2026-11-15

## Context

1. **Non-Django stacks lack a cohesive full-stack test harness, while Django's harness duplicates work:**
   - Frameworks such as **Flask + Alembic**, **FastAPI**, **Node/TS (Hono, Express, SvelteKit, Next.js + Drizzle/Prisma)**, **PHP (Slim, Symfony, Laravel)**, **Go**, and **Rust (`axum` + `sqlx`)** lack a built-in equivalent of `pytest-django`'s `live_server` + migrated DB, forcing teams onto slow `testcontainers` or port-colliding setup scripts. Conversely, `pytest-django` creates `test_<db>`, runs migrations, boots WSGI threads, and runs `TRUNCATE` / `serialized_rollback` — 100% redundant inside CoW worlds.
2. **User-authored shell pipelines for coverage and artifacts violate DX-first ergonomics:**
   - Asking users to assemble shard paths manually via shell env interpolation in `snow.yml` and post-suite shell recipes in `slipfork.toml` leaks orchestrator plumbing into user manifests, invites fragile shell quoting bugs, and forces every project to re-implement multi-world coverage merging.
3. **M1 benchmark proved cold process startup is the bottleneck (`429 ms` cold vs `118 ms` warm):**
   - Disk CoW cloning is sub-50 ms, whereas cold `postgres` boot, cold browser launches, and per-file VFS copies dominate wall time unless engineered out of the critical path.

## Decision

> This ADR is normative for §1–§5 (the Manifest Plumbing Law, World-Owns-The-Stack, `RunnerProtocol v1`, the 5 Speed & Stability Conditions, and Built-In Typed Coverage + `SLIPFORK_OUTPUT_DIR`). `ARCHITECTURE.md` §3.5 keeps only concise Rust trait signatures, and `docs/design/target-dx.md` keeps only user-facing examples.

1. **General Manifest Law — Zero Stringly Plumbing (normative):**
   - **Slipfork configuration never contains shell pipelines, manual path assembly, or env interpolation for values the tool itself owns — if a value is needed it is a typed field, if a path is needed the tool supplies it.**
   - User-authored post-suite shell hook fields and manual artifact-path interpolation were rejected at the `0.1.0` review gate and **never ship** (Iron Law #3: zero pre-`1.0` backward compatibility). Note on `0.1.0` crate state: `crates/slipfork-dag`'s `TestSpec` currently carries one dead, never-executed string field (pinned by M1 critical tests) that predates this amendment, has no documented or user-facing CLI surface, and is deleted along with its critical-test assertions by a mandatory follow-on code node before `0.1.0` merges — with zero compatibility promise.
   - **Approved Scoped Surfaces Preserved:** Mode-scoped components (`modes.<mode>.components.<id>` add/remove/override in `snow.yml`, `ADR-0001` §1) and the test-scoped `[test]` table in `slipfork.toml` (which carries **only typed configuration fields** — no hook surface exists under `[test]`) remain **approved as designed**.
2. **World-Owns-The-Stack Inversion (OSPP-Independent):**
   - The CoW world — defined once by `snow.yml` (`resources`, `hooks: pre-deploy`, `components`, `ingresses`) plus `slipfork.toml` (`[artifacts]`, `[fixtures]`) — migrates PostgreSQL, seeds origin-independent snapshots, assigns the world's single `WorldOrigin` (`http://test-<slot>.<project>.localhost`, `ADR-0005` §1), runs any `per_world = true` fixtures, and boots live HTTP/worker services before any test runs. **OSPP is not a prerequisite for M2 or M3** (`FileVaultProvider` + derived `.localhost` origins suffice).
   - Non-Django stacks read Slipfork-supplied `BASE_URL` (`origin.url()`) and resolved `snow.yml` env vars (`DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`) with zero `testcontainers` or `live_server` glue; Django's `pytest-slipfork` (**Adapter #1 in M3**) subtracts `test_<db>` creation, `live_server`, and `serialized_rollback`.
3. **Canonical Per-Run Directory Tree & `RunnerProtocol v1` (`AF_UNIX` NDJSON):**
   - Every `slipfork test` invocation mints one `<run_id>` (via injected `EntropySource`) and uses **one canonical per-slot layout**: `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` (`<slot>` = `test-<u32>`).
   - `TestCoordinator` (`crates/slipfork-test`, M3) drives out-of-process language plugins through `SocketRunnerAdapter` over `.slipfork/runs/<run_id>/<slot>/runner.sock` (and generic CLIs through `CommandTestAdapter`), passing `WorldEndpoint.origin` (`WorldOrigin::url()` exposed strictly at the wire/env boundary) and verifying every `TestAdapter` against one shared conformance suite:
     - `{"version":1,"op":"discover","filters":[...]}` -> `{"op":"discovered","items":[{"id":"...","fixture":"catalog","isolation":"world"}]}`
     - `{"version":1,"op":"run","world_id":"test-3","base_url":"http://test-3.acme.localhost","output_dir":".slipfork/runs/run-01/test-3/output","items":[...]}` -> streams `{"op":"item_result","id":"...","status":"passed|failed|skipped","duration_ms":84.2,"message":null}` followed by `{"op":"run_complete"}`.
   - Zygote-capable language plugins (`pytest-slipfork`) pre-import the framework at startup and `fork()` per `run` request with zero Zygote-specific wire messages.
4. **The 5 Mandatory Speed & Stability Conditions:**
   1. **Rolling Pre-Warmed CoW World Pool:** Background replenisher boots `W` cloned worlds (`postgres` + app servers + `test-<slot>.<project>.localhost` route) ahead of worker demand.
   2. **Shared Host Browser + Per-Test `BrowserContext` over `http://test-<slot>.<project>.localhost`:** One warm host browser creates isolated `< 5 ms` `BrowserContext`s against port-80 `slipforkd` routes.
   3. **Two-Tier `batch` vs `world` Scheduling:** Read-only / SAVEPOINT tests share a warm world (`isolation = "batch"`); committed-SQL / browser / worker / S3 tests consume dedicated CoW worlds (`isolation = "world"`) with `0 ms` teardown.
   4. **O(1) Volume Cloning (`VolumeCloner`):** ZFS/Btrfs dataset clones (`IncusEngine`) or sparse loopback-XFS / `FICLONE` volume pools (`ReflinkNetnsEngine`).
   5. **Hermetic `CHECKPOINT` + Readiness Sealing:** Every reusable `[fixtures.<id>]` snapshot flushes `CHECKPOINT;` cleanly before sealing so clones never replay crash-recovery WAL.
5. **Built-In Typed Coverage (`TestAdapter` Capability) & Surviving `SLIPFORK_OUTPUT_DIR` Orthogonality (M3):**
   - **Typed `[test]` Intent — One Canonical Activation Rule (`slipfork.toml`):**
     ```toml
     [test]
     coverage = true
     coverage_report = ["html", "xml"]
     coverage_fail_under = 85        # optional (validated CoverageThreshold in 0..=100)
     ```
     `coverage = true` is the **sole switch** enabling coverage (`coverage_report` defaults to `["text"]` when omitted). Setting `coverage_report` or `coverage_fail_under` without `coverage = true` fails deserialization (`DagError::CoverageOptionsRequireEnabled`, preventing a second implicit activation syntax), and `coverage_fail_under` deserializes into `CoverageThreshold` enforcing `0..=100` (`DagError::InvalidCoverageThreshold` / `TestError::InvalidCoverageThreshold` on `> 100`). In Rust domain types, `[test]` parses into `TestSpec { pub coverage: Option<CoverageConfig> }` (`None` when disabled; zero redundant boolean flag in domain structs).
   - **`TestAdapter` Typed Coverage Contract (capability + world env + collect):**
     1. **Capability:** `TestAdapter::coverage(&self) -> Option<&dyn CoverageDriver>`. When `TestSpec.coverage` is `Some(_)` and the active adapter returns `None` (or its required host tool is missing), `TestCoordinator` fails closed (`TestError::CoverageUnsupported`) before booting worlds.
     2. **Slot-Stable Pre-Warm World Env:** When coverage is enabled for a run, each pre-warmed world `test-<slot>` boots directly with its slot-stable directory `.slipfork/runs/<run_id>/test-<slot>/coverage` bind-mounted read-write and `CoverageDriver::world_env` applied in `WorldSpec.base_env` (Python `COVERAGE_FILE` + runner `--cov` shards; Node `NODE_V8_COVERAGE`; PHP harness-supplied clover/pcov path; Rust `LLVM_PROFILE_FILE`) — adding `0 ms` on slot adoption rather than replaying `spawn_service` (`~300 ms`), which would defeat Condition #1.
     3. **Orchestrator Collect:** After `N` worlds finish, `CoverageDriver::collect(&self, shard_dirs: &[PathBuf], formats: &BTreeSet<CoverageReportFormat>, report_dir: &Path) -> Result<CoverageSummary, TestError>` merges shards inside `TestCoordinator` via typed process `argv` (never a shell), and `TestCoordinator` enforces `CoverageThreshold` uniformly across all adapters.
   - **Blastit / Allure / Playwright Traces via Tool-Owned `SLIPFORK_OUTPUT_DIR`:** Slipfork mounts `.slipfork/runs/<run_id>/<slot>/output` read-write into each test world and runner as `SLIPFORK_OUTPUT_DIR` and preserves it across world teardown. External suites (Blastit `blastit dev -- ...` in `modes.local.components.<id>.run_command`, Playwright traces, Allure) write to `SLIPFORK_OUTPUT_DIR` through their own native tooling with zero shell-pipeline config in `slipfork.toml` and zero tool-specific collector traits inside Slipfork.

## Consequences

- Coverage across `N` parallel multi-component CoW worlds works from one typed `[test]` schema with zero shell plumbing, zero `spawn_service` restart penalty on pre-warmed slots, and one canonical `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` directory layout.
