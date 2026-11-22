---
id: M3.58
title: "Run-ID log model (slipfork logs --run <id>) for build/setup/hook runs, slirp4netns outbound world networking & single-✗ error formatting"
kind: epic
status: done
priority: 3
depends_on: [M3.57]
paths: ["crates/**", "specs/**", "docs/**", "packaging/**", "scripts/**", ".github/workflows/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T21:46"
updated: "2026-11-22T23:36"
---

# Goal

Fix the conceptual log-attachment flaw exposed by `slipfork logs uv sync` (`✗ ✗ 'uv' is not running and has no captured logs`), fix world outbound internet (`slirp4netns` so `@uv` `uv sync`, `@pnpm` `pnpm install`, and running services can reach PyPI/npm/HTTPS APIs inside the world `netns`), and eliminate the double `✗ ✗` error prefix:

1. **First-Class `run_id` Log Model (`slipfork logs --run <run_id>` / `-r <run_id>`):**
   - **Conceptual bug:** One-shot DAG executions (`Dockerfile` image builds, `@uv`/`@pnpm` setup steps like `uv sync`, `before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`) are **NOT** long-lived services — pretending `"uv sync"` was a service target produced `check logs with: slipfork logs uv sync` (which split on the space into `'uv'` and `'sync'` and failed with `'uv' is not running`!).
   - **Clean model:**
     - Every `slipfork up`, `slipfork fork`, or `slipfork restart` invocation generates a short, unambiguous, space-free **`run_id`** (e.g. 6-char hex/alphanumeric like `a4f9c2`), persisted under `<store>/logs/<world>/runs/<run_id>.log` + `.idx` (recording each step header `── setup backend (uv sync) ──` + timestamped stdout/stderr lines, plus updating `<store>/logs/<world>/runs/last` / `last-failed`!).
     - Long-lived pods (`api`, `worker`, `front`, `mailpit`, `pg`, `cache`) keep their per-service `<store>/logs/<world>/<service>.log` + `.idx` streams for `slipfork logs [SERVICES]...`.
     - Add **`slipfork logs --run <run_id>`** (`-r <run_id>`, accepting `<run_id>`, `last`, or `failed`!).
     - Whenever **any** DAG step fails during `up` / `fork` / `restart`, print:
       `✗ setup     backend (uv sync)   failed   112ms`
       `✗ Failed to run setup 'uv sync' (image backend) — check logs with: slipfork logs --run <run_id>`
       Prove in an E2E test that copying and running the exact printed `slipfork logs --run <run_id>` command exits `0` and displays the failed step's real stderr!

2. **World Outbound Internet via `slirp4netns` (`crates/slipfork-world`, `packaging/`, `.github/workflows/`):**
   - **Why `uv sync` failed right after `image backend built 7.3s`:** `Dockerfile` `RUN` had host networking, whereas `slipfork-world-init` ran inside `unshare --net` with ONLY `127.0.0.1` loopback and **zero outbound route** (`ENETUNREACH`) — breaking `@uv` (`uv sync`), `@pnpm` (`pnpm install`), AND any service calling external HTTPS APIs (`specs/03` promises outbound internet from world pods!).
   - Attach **`slirp4netns --configure --mtu=65520 --disable-host-loopback <init_pid> tap0`** (supervised alongside `slipfork-world-init`, reaped cleanly on world stop, using `10.0.2.3` SLIRP DNS in pod `resolv.conf` or upstream `resolv.conf`!) so every pod in the world has working outbound TCP/UDP/DNS while `127.0.0.1` stays 100% isolated per world and `--disable-host-loopback` prevents pods from reaching host `127.0.0.1`!
   - Add `slirp4netns` to `slipfork doctor` (`Containers` check), `.github/workflows/ci.yml` (`live` job `apt-get install`), `.github/workflows/packages.yml` (`verify` containers where needed), and `packaging/{deb,rpm,arch,alpine}` (`Depends`/`Requires` alongside `crun`)!

3. **Eliminate Double `✗ ✗` Error Prefix:**
   - Fix the error-formatting path that produced `✗ ✗ 'uv' is not running...` — only ONE boundary renders the leading `✗ `, and add a regression test asserting zero `✗ ✗` across error paths!

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` all exit `0`.
- Live verification on `165.232.86.174`:
  1. `@uv` (`uv sync`) and `@pnpm` (`pnpm install`) succeed from a clean checkout inside the world `netns` over `slirp4netns`.
  2. Triggering a deliberate setup/build/hook failure prints `check logs with: slipfork logs --run <run_id>`, and running `slipfork logs --run <run_id>` prints the exact failure output with zero `✗ ✗`.
