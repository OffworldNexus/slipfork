# ADR-0006: World-Owns-The-Stack Inversion, Polyglot `RunnerProtocol v1`, and Pure `after_suite` Orthogonality

- **Status:** Accepted (normative specification for `RunnerProtocol v1`, world-owned-stack testing, and `after_suite` orthogonality)
- **Date:** 2026-11-14

## Context

1. **Non-Django stacks lack a cohesive full-stack test harness, while Django's harness duplicates work:**
   - Frameworks such as **Flask + Alembic**, **FastAPI**, **Node/TS (Hono, Express, SvelteKit, Next.js + Drizzle/Prisma)**, **PHP (Slim, Symfony, Laravel)**, **Go**, and **Rust (`axum` + `sqlx`)** have no built-in equivalent of `pytest-django`'s `live_server` + migrated test DB. Teams bolt on slow Docker `testcontainers` or fragile multi-process `conftest.py` / `globalSetup.ts` scripts that collide on ports and omit background workers or S3.
   - Conversely, Django (`pytest-django`) creates `test_<db>` from scratch, runs migrations, spawns in-process WSGI threads on random ports, and executes `TRUNCATE` / `serialized_rollback` after every test — work that is 100% redundant once CoW worlds exist.
2. **Coupling `slipfork-test` to `pytest` or Blastit violates IoC orthogonality:**
   - Hardcoding `pytest` CLI semantics limits polyglot stacks, while embedding a Blastit-specific observation collector trait or custom CLI flags inside Slipfork couples the orchestrator to one security tool's wire schema when coverage, Allure, Playwright traces, and Blastit all share the exact same lifecycle need: **per-world output directory + single `after_suite` aggregation hook**.
3. **M1 benchmark proved cold process startup is the bottleneck (`429 ms` cold vs `123 ms` warm):**
   - Disk CoW cloning is cheap, whereas cold `postgres` startup, cold `chromium.launch` per world, and per-file VFS cloning dominate wall time unless engineered out of the critical path.

## Decision

> This ADR is normative for §1–§4 (`RunnerProtocol v1`, the 5 Speed & Stability Conditions, and `$SLIPFORK_ARTIFACTS_DIR` / `[test].after_suite` mechanics). `ARCHITECTURE.md` §3.5 keeps only concise Rust trait signatures, and `docs/design/target-dx.md` keeps only user-facing examples and recipes; neither restates these protocol schemas or lifecycle invariants.

1. **World-Owns-The-Stack Inversion:**
   - The CoW world — defined once by `snow.yml` (`resources`, `hooks: pre-deploy`, `components`, `ingresses`) plus `slipfork.toml` (`[artifacts]`, `[fixtures]`) — **already** migrates PostgreSQL, seeds fixtures, and boots the live HTTP/worker stack before any test executes.
   - Non-Django frameworks (Flask, FastAPI, Hono/SvelteKit, Symfony/Slim, Go, Rust) become **zero-boilerplate first-class citizens**: tests simply read `BASE_URL` (`http://test-<slot>.<project>.localhost`) and resolved `snow.yml` env vars (`DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`) against a live multi-component world — zero `testcontainers`, zero custom `live_server` threads.
   - Django's `pytest-slipfork` plugin (**Adapter #1 in M3**) is a thin subtraction plugin: it disables `pytest-django`'s `test_<db>` creation, `live_server`, and `serialized_rollback`, pointing Django directly at the world's CoW PostgreSQL (`127.0.0.1:5432`) and `$BASE_URL`.
2. **Single Canonical `RunnerProtocol v1` (`AF_UNIX` NDJSON) + Shared `TestAdapter` Conformance:**
   - `TestCoordinator` (`crates/slipfork-test`) drives out-of-process language plugins through `SocketRunnerAdapter` over `$SLIPFORK_ARTIFACTS_DIR/runner.sock` (and arbitrary CLIs like `go test` through `CommandTestAdapter`), with every `TestAdapter` verified against one shared conformance suite:
     - `{"version":1,"op":"discover","filters":[...]}` -> `{"op":"discovered","items":[{"id":"...","fixture":"catalog","isolation":"world"}]}`
     - `{"version":1,"op":"run","world_id":"test-3","base_url":"http://test-3.acme.localhost","artifacts_dir":".slipfork/test-artifacts/run-01/test-3","items":[...]}` -> streams `{"op":"item_result","id":"...","status":"passed|failed|skipped","duration_ms":84.2,"message":null}` followed by `{"op":"run_complete"}`.
   - Zygote-capable language plugins (`pytest-slipfork`) pre-import the framework at process startup and `fork()` internally on each `run` request — requiring zero Zygote-specific wire messages.
3. **The 5 Mandatory Speed & Stability Conditions:**
   1. **Rolling Pre-Warmed CoW World Pool:** Background replenisher boots `W` cloned worlds (`postgres` + app servers + `test-<slot>.<project>.localhost` route) ahead of worker demand.
   2. **Shared Host Browser + Per-Test `BrowserContext` over `http://test-<slot>.<project>.localhost`:** One warm host Chromium/Firefox/WebKit instance creates isolated `< 5 ms` `BrowserContext`s hitting port-80 `slipforkd` hostnames instead of cold-launching browsers per world.
   3. **Two-Tier `batch` vs `world` Scheduling:** Fast read-only / SAVEPOINT tests share a warm world (`isolation = "batch"`), while committed-SQL / browser / worker / S3 tests consume dedicated CoW worlds (`isolation = "world"`) with `0 ms` teardown.
   4. **O(1) Volume Cloning (`VolumeCloner`):** ZFS/Btrfs dataset clones (`IncusEngine`) or sparse loopback-XFS / `FICLONE` volume pools (`ReflinkNetnsEngine`) avoid O(files) copy overhead.
   5. **Hermetic `CHECKPOINT` + Readiness Sealing:** Every `[fixtures.<id>]` snapshot flushes `CHECKPOINT;` cleanly before sealing so clones never replay PostgreSQL crash-recovery WAL.
4. **100% Tool Orthogonality via `$SLIPFORK_ARTIFACTS_DIR` + `[test].after_suite`:**
   - Slipfork contains zero Blastit-specific or tool-specific collector traits or CLI flags.
   - Each test world mounts a surviving host directory `.slipfork/test-artifacts/<run_id>/<world_id>/` (`<run_id>` minted via injected `EntropySource`) read-write as `$SLIPFORK_ARTIFACTS_DIR` inside both `snow.yml` components and the test runner, surviving CoW world destruction.
   - `SuiteHookRunner::run_after_suite` (`ShellSuiteHookRunner`) executes `slipfork.toml` `[test].after_suite` once via `sh -eu -c <command>` in `repo_root` (inheriting host `PATH`/`HOME` so tools locate user installs, with `SLIPFORK_RUN_ARTIFACTS_DIR=.slipfork/test-artifacts/<run_id>`) **on both pass and fail** without masking a non-zero test exit code. Coverage (`coverage combine`), Allure, Playwright traces, and Blastit (`run_command: blastit dev ...` in `snow.yml` + merge/check in `after_suite`) all consume this single generic mechanism.

## Consequences

- Polyglot stacks get instant full-stack integration and browser tests without `testcontainers` or `live_server` glue; Slipfork stays 100% decoupled from Blastit and `pytest` internals while hitting sub-150 ms effective browser+DB test latency.
