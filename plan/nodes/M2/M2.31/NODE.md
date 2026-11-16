---
id: M2.31
title: "Fix poc/run_benchmark.py for ADR-0009 host-browser/.localhost topology and seeded base snapshot; verify bounded 1x1 --check"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M2.30]
paths: ["poc/**", tools/tests/test_run_benchmark.py]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T03:04"
updated: "2026-11-16T05:38"
---

# Goal

Align `poc/run_benchmark.py` with `ADR-0009` §2.2 (host Chromium over `.localhost` `WorldOrigin`, lean app `Dockerfile`) and `M2.30`'s `OSPP v1` seed-dump + `freeze_world` base snapshot so the bounded `1x1` `poc/run_benchmark.py --check` exits `0` deterministically without hanging or mutating tracked files.

# Context

- Independent M2 milestone gate verification proved `uv run --no-project poc/run_benchmark.py --check --num-tests 1 --workers 1` times out (`RC=143` > 15 min) and had a broken fork-seed invariant:
  1. `poc/run_benchmark.py:159-163` installed full Chromium **inside** the sample `Dockerfile` (`python -m playwright install --with-deps chromium`, ~1.5 GiB extracted rootfs rebuilt after every `prepare_run_state` wipe), violating `ADR-0009` §2.2 (*"Headless browsers remain in the shared host browser pool over `.localhost`"*).
  2. `poc/run_benchmark.py:998-1006` seeded `10,000` products into the live `canonical` world post-boot via `slipfork exec` instead of seeding `<project>-base`, so `slipfork fork` cloned an unseeded snapshot.

# Design / Invariants

1. **Lean `L0` App Image + Real `.localhost` Ingress + Host Chromium (`ADR-0009` §2.2):**
   - Sample `Dockerfile` uses `FROM python:3.12-slim-bookworm AS app` installing **only** `psycopg[binary]` (zero in-world Playwright/Chromium).
   - `SNOW_YML` `components.api` runs the real Python HTTP checkout server on `127.0.0.1:8000` (`internal_ports: [8000]`, `ingresses: / -> api:8000`), so `slipfork up` and `slipfork fork` wait on real `:8000` readiness.
   - Cold and pre-warmed suite drivers run Playwright (`CHROME_BIN`) **on the host** against each world's single `.localhost` `WorldOrigin` URL returned by `slipfork --json up` / `slipfork --json fork` (exercising `slipforkd` `.localhost` HTTP ingress + `dial_world_tcp` end-to-end!).
2. **Real Seeded `<project>-base` Snapshot via `M2.30`:**
   - Compress `seed-catalog.pgdump` (produced in `measure_restore_pipeline` by real PostgreSQL 18 `pg_dump -Fc`) with `zstd` and pair/serve it to `slipforkd` over a minimal loopback `ospp-v1` seed server in `poc/` (or `slipfork auth login -` -> `slipfork up`), so `slipforkd` restores `seed-catalog.pgdump` inside `postgres:18` and seals `<project>-base` via `freeze_world`. Every subsequent `slipfork fork` CoW-clones the 41.5 MiB seeded `PGDATA` (`10,000` products, `200` tenants, `5,000` audit events) in milliseconds with zero SQL re-seed and zero direct state-dir manipulation.
3. **Clean Process Teardown & Read-Only `--check`:**
   - Ensure the `finally:` block terminates the run's auto-spawned `slipforkd` (`daemon.sock` / process group) so zero orphan `slipforkd` or `slipfork-world-init` processes linger after `run_benchmark.py` exits.
   - Keep `docs/research/m1-poc-benchmark.md` and `poc/bench-baseline.json` untouched under `--check`; update `tools/tests/test_run_benchmark.py` for any changed helper signatures.

# Acceptance

```bash
make py-lint
make py-test
cargo build --release -p slipfork -p slipfork-engine --bins
uv run --no-project poc/run_benchmark.py --check --num-tests 1 --workers 1 --run-dir ~/.cache/slipfork/bench-verify
git diff --exit-code docs/research/m1-poc-benchmark.md poc/bench-baseline.json
! pgrep -f "bench-verify"
make check
```

All commands exit `0`, and the bounded `1x1` `--check` run finishes cleanly in under 180 seconds with zero leaked `bench-verify` processes and zero tracked file drift.
