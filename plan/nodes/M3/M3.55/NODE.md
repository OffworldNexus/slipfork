---
id: M3.55
title: "Upgrade support/demo & live /home/slipfork/demo on 165.232.86.174 to a real Python (uv + FastAPI) & SvelteKit (pnpm) full-stack app"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.54]
paths: ["support/demo/**", "docs/**", README.md, "crates/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T20:59"
updated: "2026-11-22T21:00"
---

# Goal

Replace the minimal toy demo in `support/demo` and `/home/slipfork/demo` on `165.232.86.174` with a genuinely representative, interactive modern polyglot application using **Python (`uv` + FastAPI)** for the backend/worker and **SvelteKit (`pnpm` + Vite)** for the frontend:

1. **Backend (`support/demo/` — Python 3.13 + `uv` + FastAPI):**
   - Real `pyproject.toml` + committed `uv.lock` (`fastapi`, `uvicorn`, `psycopg[binary]`, `redis`, `httpx` / S3 client) + clean `Dockerfile` (`FROM python:3.13-slim`, installing `uv`).
   - Uses **`@uv`** in `Forkfile`:
     - `pre-deploy: @uv run python -m app.migrate` (idempotent Postgres `app_db` schema + sample rows + seeding a public SVG/asset into `assets` and a private JSON audit blob into `uploads`).
     - `service api`: `port 8000`, `route /api`, `run: uv run --no-sync uvicorn app.main:app --host 0.0.0.0 --port $port`, `on dev: run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port` — serving JSON endpoints (`/api/status`, `GET/POST /api/items`, `POST /api/test-email` sending a real SMTP test message to `mailpit` when `SMTP_ADDR` is set, and S3 upload/check endpoints!).
     - `service worker`: `@uv run python -m app.worker` processing background jobs from Redis/Postgres cleanly.

2. **Frontend (`support/demo/front/` — SvelteKit + `pnpm` + Vite):**
   - Real SvelteKit 2 + Svelte 5 project (`package.json`, committed `pnpm-lock.yaml`, `svelte.config.js` with `@sveltejs/adapter-node`, `vite.config.ts` allowing `*.localhost` hosts so Vite 6's `server.allowedHosts: ['.localhost']` accepts `demo.localhost` and `feat-auth--demo.localhost` without 403 host blocking!, `src/routes/+page.svelte`) + clean `Dockerfile` (`FROM node:22-slim` + `corepack enable`).
   - Uses **`@pnpm`** in `Forkfile`:
     - Production `run`: `node build` (`port 4173`, `route /`)
     - `on dev`: `port 5173`, `run: @pnpm exec vite dev --host 0.0.0.0 --port $port`
   - Crisp, modern interactive SvelteKit UI showing:
     - Active world / git branch badge + Postgres/Redis/S3 health indicators
     - Interactive task/item list backed by FastAPI + PostgreSQL 18 (so adding an item in `feat-auth` vs `demo` visually proves instant Btrfs database isolation!)
     - S3 public asset preview (`http://assets.<world>.localhost/...`) + "Send test email" button delivering to `http://mail.<world>.localhost`!

3. **Deploy & Prove Live on `165.232.86.174` (`/home/slipfork/demo`):**
   - Sync the new `support/demo` into `/home/slipfork/demo` on `165.232.86.174` (preserving Btrfs subvolume at `/home/slipfork/demo`, clean git commit on `main`), run `slipfork up` (proving `@uv` `uv sync` + `@pnpm` `pnpm install` auto-setup nodes!), create a live fork `slipfork fork feat-auth` (proving `.venv` + `node_modules` snapshot via Btrfs CoW with `(up to date)` `0ms` installs and isolated DB rows!), and verify `http://demo.localhost/`, `http://demo.localhost/api/status`, `http://feat-auth--demo.localhost/`, `http://mail.demo.localhost/`, `http://assets.demo.localhost/` (`200`), and `http://uploads.demo.localhost/` (`403` anon) all respond cleanly over `:80`!

# Acceptance

- `make check` & `make docs-check` (`check_docs_honesty.py` checks `support/demo/README.md`!) exit `0`.
- Live `/home/slipfork/demo` + `feat-auth` fork on `165.232.86.174` run the real FastAPI (`@uv`) + SvelteKit (`@pnpm`) + Postgres 18 + Redis 7 + S3 + Mailpit stack cleanly on `:80`, with Vite serving `http://demo.localhost` and `http://feat-auth--demo.localhost` (`200`, zero Vite `Blocked request. This host is not allowed` error!).
