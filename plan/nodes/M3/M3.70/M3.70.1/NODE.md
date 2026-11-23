---
id: M3.70.1
title: "demo ./api + ./front, pure Dockerfile CMD in !dev, specs/docs sync"
kind: task
status: review
owner: implementer
priority: 3
depends_on: [M3.69]
paths:
  - "support/demo/**"
  - "crates/slipfork/tests/**"
  - "specs/**"
  - "docs/**"
  - README.md
  - "packaging/man/**"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T13:30"
updated: "2026-11-23T13:47"
---

# Goal

Restructure `support/demo` into symmetrical `./api` + `./front`, make `!dev` run pure Dockerfile `CMD`s, and sync specs/docs/README. Consumes the `M3.69` CLI (`-F/--features`, `slipfork features`, built-in `dev`).

# Scope

1. Move `support/demo/{Dockerfile,pyproject.toml,uv.lock,app}` -> `support/demo/api/`; delete the untracked `support/demo/front/build/` directory.
2. `support/demo/front/Dockerfile`: `FROM node:22-slim AS dev` (`RUN npm install -g pnpm@10`, `WORKDIR /project/front`); `FROM dev AS prod` (`WORKDIR /app`, `COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./`, `RUN pnpm install --frozen-lockfile`, `COPY . .`, `RUN pnpm run build`, `ENV HOST=0.0.0.0 PORT=3000`, `EXPOSE 3000`, `CMD ["node", "build"]`).
3. `support/demo/api/Dockerfile`: `FROM python:3.13-slim AS dev` (`ENV PYTHONUNBUFFERED=1`, `RUN python -m pip install --no-cache-dir uv`, `WORKDIR /project/api`); `FROM dev AS prod` (`WORKDIR /app`, `COPY pyproject.toml uv.lock ./`, `RUN uv sync --frozen --no-dev`, `COPY . .`, `ENV PATH="/app/.venv/bin:$PATH"`, `EXPOSE 8000`, `CMD ["sh", "-c", "uvicorn app.main:app --host 0.0.0.0 --port ${PORT:-8000}"]`).
4. `support/demo/Forkfile`: `features { dev = [mail]; mail }`; `image backend ./api` (`on dev: stage dev`, `service api` with `port 8000`, `route /api`, `on dev { pre-deploy: uv run python -m app.migrate; run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port } else: pre-deploy: python -m app.migrate`, plus `service worker { on dev: run: @uv run python -m app.worker; else: run: python -m app.worker }`); `image frontend ./front` (`on dev: stage dev`, `service front { port 3000; route /; on dev { port 5173; run: @pnpm exec vite dev --host 0.0.0.0 --port $port } }`) with ZERO `else` block; the `on mail` mailpit block unchanged.
5. `crates/slipfork/tests/critical_fork_live.rs:305` `demo_files()` -> `api/Dockerfile`, `api/pyproject.toml`, `api/uv.lock`, `api/app/*.py` (keep front paths).
6. Docs/specs: `specs/01-forkfile.md` §3 (built-in `dev`, default base, `-dev`), `specs/02-cli-ux.md` (`slipfork features`, `-F/--features`, `--no-default-features`, dev-only `/project` bind vs pure Dockerfile `CMD` in `!dev`), `docs/forkfile.md`, `docs/cli.md`, `support/demo/README.md`, `README.md`; regenerate `packaging/man/slipfork.1` if the CLI changed since M3.69.2.

# Acceptance

- `make check` and `make docs-check` exit `0` (zero vaporware; `tools/check_docs_honesty.py` sees only shipped verbs).
- `cargo test -p slipfork --test critical_fork_live --no-run` compiles with the new fixture list.
