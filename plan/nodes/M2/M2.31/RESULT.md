# M2.31 RESULT — `poc/run_benchmark.py` host-browser `.localhost` topology + seeded base snapshot

**Status:** complete. Scope touched: `poc/run_benchmark.py`, `tools/tests/test_run_benchmark.py`
(`Makefile` unchanged — `make bench` already builds both bins and runs `--check`). Zero crate edits.

## What changed

1. **Lean `L0` app image + real ingress + host Chromium (`ADR-0009` §2.2)**
   - `DOCKERFILE` is now `FROM python:3.12-slim-bookworm AS app` + `pip install "psycopg[binary]"`
     only (zero Playwright/Chromium in-world), `EXPOSE 8000`.
   - `components.api` runs the real `api/app.py` HTTP checkout server (`internal_ports: [8000]`,
     `ingresses: / -> api:8000`), so `up`/`fork` wait for real `:8000` readiness in the world netns.
   - Cold/warm suites launch Playwright (`SLIPFORK_BENCH_CHROME`) **on the host** against the
     world's single `.localhost` `WorldOrigin` from `slipfork --json up|fork` (asserts the hostname
     ends in `.localhost`, so `slipforkd` ingress + `dial_world_tcp` are exercised).
   - The traditional baseline keeps host PG18 + host Chromium, but its app binds an **ephemeral**
     loopback port (a foreign developer server already held host `:8000`); worlds still bind
     canonical `:8000` in their own netns.
2. **Real seeded `<project>-base` via `M2.30` (no post-boot SQL seed)**
   - `measure_restore_pipeline` produces a real PostgreSQL 18 `pg_dump -Fc -Z zstd`
     (`run_dir/seed-catalog.pgdump`) and measures the real `pg_restore`; `compress_seed_dump`
     zstd-compresses it and pins the transport SHA-256/size.
   - New in-harness minimal loopback `ospp-v1` host (`SeedOsppServer`, `SeedDumpOffer`,
     `p256_verify`, `_WebSocket`): server-first `CHALLENGE`, fragment-only pairing, **real ECDSA
     P-256/SHA-256 prehash verification** of `pair_hello`/`HELLO`, `env.bundle`,
     `secrets.ensure`, `resources.describe` (`format: pg_custom_zstd`), and bounded
     `dump.begin -> binary frames -> dump.end`.
   - `slipfork auth login -` pairs the daemon's real device key (stdin URL, `BROWSER=/bin/true`
     pinned so a headless host never needs a desktop opener), then `slipfork up` streams the dump,
     restores it inside `postgres:18`, and seals `<project>-base` with `freeze_world`. Later
     `fork`s CoW-clone the restored `PGDATA`; the 4-world concurrency check asserts `products =
     10000` and inserts one order per world with no re-seed.
3. **Clean teardown & read-only `--check`**
   - `SlipforkDaemon` owns the real `slipforkd --listen 127.0.0.1:0` child (new session), and
     `stop()` SIGTERM/SIGKILLs its process group, then asserts no live
     `slipforkd --state-dir <run>/state` or `slipfork-world-init .*<run>` process remains.
   - Removed the direct `<state>/artifacts|fixtures/<digest>` `mkdir` fake (forbidden state-dir
     manipulation). `dag plan` is now measured as a read-only Merkle check (best of 3) and the
     invariants are digest-based: unchanged tree is deterministic; editing `front/src` changes
     `artifact:front` and leaves every DB fixture digest identical.
   - `--check` writes nothing: `docs/research/m1-poc-benchmark.md` and `poc/bench-baseline.json`
     stay byte-identical (git diff clean).
   - Measurement hygiene on a loaded host: best-of-3 `doctor` reflink probe and best-of-3
     `dag plan`; `_metric_bound` only enforces a lower bound when the baseline pins `min_factor`
     (faster-than-pinned latency is not drift; a broken seed floor still fails).

## Gate follow-up fixes (post-review)

- `_teardown()` always attempts `down`, `drop fork-probe`, `daemon.stop()`, and `server.stop()`:
  each step tolerates `BenchError`/`OSError` and returns problems instead of raising, so a failing
  teardown command can never skip the daemon/server stop. `measure_all` raises the collected
  teardown problems only when no original error is already propagating (never masking it), and
  prints them to stderr otherwise. Unit tests prove the daemon and seed-server stop run when
  `down`/`drop` fail, and when `daemon.stop()` itself fails.
- The pairing-URL assertion now proves the handle is absent before `#` and exactly
  `#handle=<64 hex>` after it (was tautological).
- The report test asserts the exact host-browser wording (`host** headless Chromium
  (CHROME_BIN)` + `shared **host** browser pool`).
- Cleanups: single `_host_test_argv()` (no dead `ready`/`go` kwargs, no duplicated argv),
  `measure_warm_pool_suite(env, project, run_dir, order, workers)` (duplicate `layout` param
  removed), dead `OSError/CalledProcessError` teardown branch removed, and `--check` /
  `--write-report` now share one `args.action` dest (`check` default) instead of an unread flag.

## Acceptance (all run on this host)

```text
$ make py-lint
uvx --offline ruff check tools poc     -> All checks passed!
uvx --offline ruff format --check tools poc -> 9 files already formatted   RC=0

$ make py-test
41 passed in 0.69s                                                        RC=0

$ cargo build --release -p slipfork -p slipfork-engine --bins
Finished `release` profile [optimized] target(s)                          RC=0

$ uv run --no-project poc/run_benchmark.py --check --num-tests 1 --workers 1 \
      --run-dir ~/.cache/slipfork/bench-verify
bench-check: OK (measured record within every committed bound; read-only)  RC=0  WALL=39 s

metric                              committed     measured  verdict
reflink_64k_us                            397          536  ok
pgdata_logical_mib                       41.5        42.01  ok
pgdata_files                             1000         1001  ok
dag_warm_plan_ms                        10.89        18.29  ok
dag_front_only_plan_ms                   10.7        19.07  ok

$ git diff --exit-code docs/research/m1-poc-benchmark.md poc/bench-baseline.json
(no output)                                                               RC=0

$ make check
plan-check, guard, py-lint, py-test, support-check, ts-test, rs-fmt, rs-lint,
rs-test (+doc), docs-check (zensical --strict + check_docs_honesty OK)      RC=0
```

Bounded-run timings (`--json`, one quiet-host run; total **25.4 s**, best observed 23.3 s; the
post-fix bounded acceptance run took 39 s with heavy foreign CPU load — all far below the 180 s
bound):

| Phase | Measured |
|---|---|
| `slipfork doctor` 64 KiB `FICLONE` (best of 3) | 431 µs |
| real `pg_dump -Fc -Z zstd` pipeline + zstd + `pg_restore` | ~2 s (restore 45 ms) |
| `slipfork up` (L0 build + `postgres:18` OCI export + streamed restore + `freeze_world`) | **13.65 s (dominant)** |
| canonical seeded-world host-browser proof (10000 products) | ~0.5 s |
| `slipfork fork` / `exec` | 428.8 ms / 10.0 ms |
| 4 parallel mutating worlds | 2.18 s |
| `slipfork dag plan` unchanged / after `front/src` edit | 12.1 ms / 11.7 ms |
| traditional baseline (1 test, 1 worker) | 1.26 s |
| cold CoW suite (1 test) / warm pool (1 test) | 1.42 s / 0.248 s |

## Teardown evidence

`slipforkd` + `slipfork-world-init` scoped to the run directory after exit (script run so the
checker's own argv cannot self-match):

```text
pgrep -af "target/release/slipforkd --listen 127.0.0.1:0 --state-dir <run>/state"
pgrep -af "slipfork-world-init .*<run>"
-> no output (zero live leftovers)
```

Caveat: the literal acceptance line `! pgrep -f "bench-verify"` also matches the *shell that
carries the pattern in its own argv* (and an unrelated stale `[bash] <defunct>` zombie under the
pty daemon). The scoped checks above are the meaningful zero-leftover evidence; the harness itself
asserts `_leftover_processes()` is empty in its `finally:` block and fails closed otherwise.

## Invariants honoured

- No in-world browser: the lean-image unit test asserts no `playwright`/`chromium` in
  `DOCKERFILE`, and the browser driver test asserts host `SLIPFORK_BENCH_CHROME` over `.localhost`.
- No fake seed path / no direct state-dir writes: dump is a real PG18 `pg_dump`, served over real
  `ospp-v1`, restored by the real daemon; the old `mkdir` cache fake is deleted.
- Fail closed with actionable diagnostics: CLI child failures, baseline failures, and host-browser
  failures surface stderr; missing `zstd`/bins/Chrome/PG18 fail in `preflight`.
- Domain logic stays behind the daemon: the harness only talks to real CLI/IPC surfaces.

## Environment note and follow-ups (out of scope, no repo files touched)

- `make check` passed (RC=0) on the final code. Two earlier attempts failed only in *crate* tests
  (`critical_slug` daemon "connection reset by peer"; `critical_cli`
  `database system is starting up`) while the host carried ~190 orphaned
  `slipfork-world-init` helpers leaked by previous test sessions plus heavy foreign CPU load
  (load average ~16). Both tests pass standalone; after reaping the orphaned helpers under
  `~/.cache/sf*` (host hygiene only, no run state touched) `make check` returned RC=0.
- FOLLOWUPS (pre-existing, outside `poc/**`): (1) the engine/CLI critical tests leak
  `slipfork-world-init` processes on this host (~190 per full `cargo test` run); (2)
  `ensure_database()` races `postgres` crash recovery because TCP `readiness_port` accepts before
  `psql` can connect (`critical_cli` flake under load).
