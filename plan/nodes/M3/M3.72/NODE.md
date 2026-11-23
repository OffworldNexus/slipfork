---
id: M3.72
title: "Collapse demo api/worker onto one 'uv run …' command & make every launched 'uv run' pass --no-sync (never sync/install dev deps in a prod pod)"
kind: task
status: active
owner: implementer
priority: 3
depends_on: []
paths:
  - "support/demo/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork/**"
  - tools/do_vm.py
  - TESTING_PLAN.md
  - "specs/**"
  - "docs/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T19:02"
updated: "2026-11-23T19:11"
---

# Goal

The `api`/`worker` `else` branches in `support/demo/Forkfile` exist only because `ServiceLaunch::resolve` (`crates/slipfork/src/world/runtime/helpers.rs:151-170`) maps the container workdir back to a **host** `/project` bind; in prod the workdir is `/app`, `host_workdir()` returns `None`, and the wrapper fast path bails. The documented fallback is already correct (`crates/slipfork/src/world/runtime/helpers.rs:401-405`: `@uv run python -m app.worker` -> `/bin/sh -c "uv run --no-sync python -m app.worker"`), which resolves `/app/.venv` from cwd inside the built image.

So the demo must call the toolchain through `uv` **once** for both modes, keeping only the genuine dev-only difference.

# Changes

1. `support/demo/Forkfile`, `image backend ./api`:
   - `service api`: `port 8000`, `route /api`, `pre-deploy: uv run python -m app.migrate`, `run: @uv run uvicorn app.main:app --host 0.0.0.0 --port $port`, and only `on dev: run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port` (DELETE the `} else: pre-deploy: python -m app.migrate` branch!).
   - `service worker`: `run: @uv run python -m app.worker` unconditional (DELETE the `on dev { … } else { run: python -m app.worker }` block!).
   - `image frontend ./front` keeps `on dev { port 5173; run: @pnpm exec vite dev … }` with zero `else` (prod `node build` comes from the image `CMD`) — dev server vs built server is a genuine difference.
2. Prove the `@uv run` prod fallback is real and quiet: assert in `crates/slipfork/tests/critical_fork_live.rs` (or the appropriate lab test) that a `-F -dev` plan carries `command: Some("@uv run … ")` for `api`/`worker` (NOT `command: None`), and that the prod world returns `200` on `/` and `/api/status` with no `mailpit` and no `/project` bind, with `uv run --no-sync` doing zero sync work.
3. Update `TESTING_PLAN.md:55` (it currently claims `command: None` on `api` and the explicit `python -m app.worker` `else: run:` for `worker`) and any `specs/01-forkfile.md` / `docs/` snippet that shows a `python -m …` prod branch. `make docs-check` must pass.
4. Live verification on `165.232.86.174` as user `slipfork` (`SLIPFORK_TEST_ROOT=/var/lib/slipfork/t`):
   - `critical_world_journey`, `critical_fork_live`, `critical_status_live`.
   - Rebuild + install the `glibc <= 2.34` `.deb`, sync `/home/slipfork/demo/Forkfile`, then `slipfork fork prod-check -F -dev` and assert `200` on `/` + `/api/status`, `worker` running, no `mailpit`, `drop -f` clean; then bring `demo` + `feat-auth` back up in dev mode (`200` on `:80`) with `tralala` stopped.

# Acceptance

- `make check` and `make docs-check` exit `0`.
- Live `prod-check -F -dev` boots `api` + `worker` through `@uv run` with `200`s and zero sync, and the dev world is unchanged.
