---
id: M3.70
title: "Restructure support/demo into ./api + ./front, use pure Dockerfile CMD in !dev, update specs/docs & verify dev + -F -dev live on VM"
kind: epic
status: ready
priority: 3
depends_on: [M3.69]
paths:
  - "crates/slipfork/**"
  - "support/demo/**"
  - "packaging/man/**"
  - "specs/**"
  - "docs/**"
  - README.md
  - TESTING_PLAN.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T13:07"
updated: "2026-11-23T13:30"
---

# Goal

Restructure `support/demo` into symmetrical `./api` and `./front` directories, clean up `support/demo/Forkfile` and both `Dockerfile`s so `!dev` uses pure `Dockerfile` `CMD` behavior (`else` removed from `front`!), update `specs/` + `docs/` + `check_docs_honesty.py` (if needed — check `docs/` CLI coverage for `features` / `-F`), and verify BOTH `dev` and `-F -dev` live on `165.232.86.174`!

# Scope

1. **`support/demo/` Symmetrical `./api` + `./front` Layout:**
   - Move `support/demo/{Dockerfile,pyproject.toml,uv.lock,app}` into **`support/demo/api/`** (`image backend ./api`).
   - Delete tracked build artifacts `support/demo/front/build/` completely (only source files belong in `support/demo/front/`)!
   - **`support/demo/front/Dockerfile`:**
     - `FROM node:22-slim AS dev` (`RUN npm install -g pnpm@10`, `WORKDIR /project/front`)
     - `FROM dev AS prod` (`WORKDIR /app`, `COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./`, `RUN pnpm install --frozen-lockfile`, `COPY . .`, `RUN pnpm run build`, `ENV HOST=0.0.0.0 PORT=3000`, `EXPOSE 3000`, `CMD ["node", "build"]`)
   - **`support/demo/api/Dockerfile`:**
     - `FROM python:3.13-slim AS dev` (`ENV PYTHONUNBUFFERED=1`, `RUN python -m pip install --no-cache-dir uv`, `WORKDIR /project/api`)
     - `FROM dev AS prod` (`WORKDIR /app`, `COPY pyproject.toml uv.lock ./`, `RUN uv sync --frozen --no-dev`, `COPY . .`, `ENV PATH="/app/.venv/bin:$PATH"`, `EXPOSE 8000`, `CMD ["sh", "-c", "uvicorn app.main:app --host 0.0.0.0 --port ${PORT:-8000}"]`)
   - **`support/demo/Forkfile`:**
     - `features { dev = [mail]; mail }` (no `default = [dev]` boilerplate needed since `dev` is built-in by default!)
     - `image backend ./api { use env common; on dev: stage dev; service api { port 8000; route /api; on dev { pre-deploy: uv run python -m app.migrate; run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port } else { pre-deploy: python -m app.migrate } } service worker { on dev: run: @uv run python -m app.worker; else: run: python -m app.worker } }`
     - `image frontend ./front { use env common; on dev: stage dev; service front { port 3000; route /; on dev { port 5173; run: @pnpm exec vite dev --host 0.0.0.0 --port $port } } }` — **ZERO `else` block in `front`!** In `!dev`, `front/Dockerfile` builds `prod` and runs `CMD ["node", "build"]` on `PORT=3000`!
   - Update `crates/slipfork/tests/critical_fork_live.rs` if it copies or asserts paths in `support/demo` (`./api` instead of `.`!).
2. **Update `specs/`, `docs/`, and `README.md`:**
   - Document built-in `dev`, `-F / --features`, `--no-default-features`, `slipfork features`, fork feature inheritance, and dev-only `/project` bind-mounting vs pure `Dockerfile` `CMD` in `!dev` across `specs/01-forkfile.md`, `specs/02-cli-ux.md`, `docs/forkfile.md`, `docs/cli.md`, and `README.md` (`make docs-check` / `tools/check_docs_honesty.py` green!).
3. **Live VM Verification (`165.232.86.174`):**
   - Run `critical_world_journey`, `critical_fork_live`, and `critical_status_live` as user `slipfork` (`SLIPFORK_TEST_ROOT=/var/lib/slipfork/t`).
   - Build & install the `glibc <= 2.34` `.deb` on `165.232.86.174`, sync `/home/slipfork/demo` (`./api` + `./front` + `Forkfile`, cleaning old root `app/`/`pyproject.toml`/`Dockerfile` in `/home/slipfork/demo` and updating `feat-auth` fork!).
   - **Verify BOTH modes live on `165.232.86.174`:**
     1) Create/boot a prod-mode fork (`slipfork fork prod-check -F -dev` -> verify `mailpit` is omitted, `front` runs `node build` from `Dockerfile` `CMD` on `:3000`, `api` runs `uvicorn` from `Dockerfile` `CMD` on `:8000`, `http://prod-check--demo.localhost/` and `/api/health` return `200`, then `slipfork drop prod-check` or keep stopped)!
     2) Leave `demo` and `feat-auth` running in `dev` mode (`200` on `:80`) and `tralala` stopped!

# Acceptance

- `make check` and `make docs-check` exit `0`.
- All 3 live CLI suites pass on `165.232.86.174`, AND live `slipfork fork prod-check -F -dev` proves `!dev` builds & serves `front` + `api` from pure `Dockerfile` `CMD` with `200 OK` on `:80`!
