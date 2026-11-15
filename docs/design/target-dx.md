# Slipfork M2–M4 Target Developer Experience Specification (`docs/design/target-dx.md`)

- **Status:** Accepted Target DX Specification (`ADR-0001`..`ADR-0006`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here are promoted into published `docs/` and `README.md` **only** as each M2–M4 binary capability ships and passes conformance tests.

---

## 1. Milestone 2 Target DX: `slipfork up`, Port-80 `*.localhost` Edge (`slipforkd`), `weaved` IPC & VCS Workspaces

### 1.1 Install & One-Time Port-80 Edge Grant

Installing the workspace places `slipfork`, `slipforkd`, and `git-remote-slipfork` onto `PATH`:

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork
```

`slipforkd` binds loopback port `80` (`127.0.0.1:80` / `[::1]:80`) via either a systemd user/system socket unit (`slipforkd.socket` passing the bound FD) or a persistent sysctl drop-in (`/etc/sysctl.d/90-slipfork-port80.conf` with `net.ipv4.ip_unprivileged_port_start=80`). If port `80` is unavailable or already occupied, `slipfork doctor` and `slipfork up` fail closed with exact diagnostics — never falling back to random ports.

### 1.2 `slipfork up`, `slipfork down` & `http://(<snow-domain>.)?(<fork>.)?<project>.localhost`

```console
$ slipfork up
[secrets] generated secrets::SECRET_KEY -> local vault (~/.local/share/slipfork/vaults/acme.json)
[world]   psql:18 ready, redis:8 ready, seaweedfs:s3 ready (isolated netns)
[hook]    pre-deploy (api-migrate): uv run python manage.py migrate (410 ms)
[edge]    http://acme.localhost -> /:8000 (uid=1000:1000, git remote 'slipfork' configured)
```

| URL | Routed Target (`LocalHostRoute`) |
|---|---|
| `http://acme.localhost` | Canonical world (`WorldTarget::Canonical`), default ingress |
| `http://admin.acme.localhost` | Canonical world, `snow.yml` `domains: [{ id: admin }]` |
| `http://feat-auth.acme.localhost` | Fork `feat-auth` (`WorldTarget::UserFork`), default ingress |
| `http://admin.feat-auth.acme.localhost` | Fork `feat-auth`, domain `admin` |

- **Disjoint Fork & Domain Rules:** Normatively specified once in `ADR-0005` §1 (`WorldTarget::validate` / `validate_fork_name`).
- **Shell Semantics (`run_command`):** Component `run_command` strings execute via `sh -eu -c <run_command>` inside the component's working directory after `${{ ... }}` templates resolve and `ExecEnv` (`env_clear()` + `PATH`/`TERM`/`LANG` + world-scoped `HOME`) is applied.

### 1.3 Interactive Shells, `git-remote-slipfork` (`<fork>` + `<fork>/wip`) & Jujutsu (`jj workspace`)

```console
$ slipfork fork feat-auth               # Creates .worktrees/feat-auth (git worktree or jj workspace) + CoW world
$ slipfork shell feat-auth              # Enters interactive $SHELL directly inside feat-auth (no cd needed)

# From the canonical checkout:
$ git pull slipfork feat-auth           # Pull committed branch tip via git-remote-slipfork
$ git fetch slipfork feat-auth/wip      # Fetch synthesized commit of uncommitted + untracked edits!
$ git diff HEAD..FETCH_HEAD
```

- Git worktree `<fork>/wip` scratch-index synthesis and Jujutsu `JjWorkspaceAdapter` mechanics are specified normatively in `ADR-0005` §3.

### 1.4 Public Sharing via Tunnel Weaver (`weaved`)

```console
$ slipfork share feat-auth
[weaved] https://feat-auth-acme.tunnel.example.com -> http://feat-auth.acme.localhost:80
$ slipfork share --stop feat-auth
```

- Daemon-owned tunnel lease semantics are specified normatively in `ADR-0005` §2; the concrete `WeavedRegistrar` adapter is **M2 and gated on `tunnel-weaver` publishing `weaved`'s real IPC specification** (`H-006`).

### 1.5 Planned M2–M3 CLI Additions

| Planned Command | Milestone | Target Behavior |
|---|---|---|
| `slipfork up` / `slipfork down` | M2 | Start/stop canonical local world & register routes in `slipforkd` |
| `slipfork shell [<fork>]` | M2 | Interactive `$SHELL` inside `<fork>` (or `cwd`'s bound world) |
| `slipfork share [--stop] [<fork>]` | M2 | Acquire/release a `weaved` tunnel lease (adapter gated on `weaved`'s IPC spec) |
| `slipfork list` / `slipfork prune` | M2 | Inspect active worlds/leases; GC unreferenced snapshots & artifacts |
| `slipfork secret set <KEY>` | M2 | Write secret from `stdin` into `0600` local vault |
| `slipfork auth login <url> --project <slug>` | M2 | Authenticate against a remote `OSPP v1` / Kerfufoo endpoint |
| `slipfork test [-j N] [-- <runner...>]` | M3 | Materialize DAG, run tests across pre-warmed CoW worlds, run `after_suite` |

---

## 2. Milestone 3 Target DX: World-Owns-The-Stack Polyglot Testing (`slipfork test`)

### 2.1 World-Owns-The-Stack Inversion

Because `snow.yml` (`resources`, `hooks: pre-deploy`, `components`, `ingresses`) already describes the entire stack, Slipfork boots and seals the migrated, seeded, routed stack **before** the test runner starts, handing every test shard `BASE_URL=http://test-<slot>.<project>.localhost` plus resolved `snow.yml` env vars (`DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`):

| Stack | Without Slipfork | With `slipfork test` (M3) |
|---|---|---|
| **Flask + Alembic / FastAPI** | Custom `testcontainers`, port-0 `uvicorn` threads, per-worker `alembic upgrade head` | **Zero boilerplate:** hit `$BASE_URL` and `$DATABASE_URL`; Alembic & background workers are already live |
| **Node / TS (`Hono`, `SvelteKit`, `Next.js` + Drizzle/Prisma)** | Slow `globalSetup.ts` Docker scripts, port collisions across Vitest/Playwright workers | **Zero boilerplate:** Playwright & Vitest hit `$BASE_URL` against isolated CoW Postgres + S3 |
| **PHP (`Slim`, `Symfony`, `Laravel`), Go, Rust (`axum` + `sqlx`)** | Hand-rolled shell wrappers or unisolated shared DBs | **Zero boilerplate:** standard `phpunit`, `go test`, or `cargo nextest` shards run in clean CoW worlds |
| **Django (`pytest-slipfork`, Adapter #1)** | `CREATE DATABASE test_...`, slow `live_server`, and `serialized_rollback` after every test | **Pure subtraction:** `pytest-slipfork` disables `test_<db>` creation, `live_server`, and `serialized_rollback` |

- **Migration Precedence Rule:** When `slipfork.toml` defines `[fixtures.*]`, `[fixtures.*]` owns the stateful snapshot lineage directly; when `slipfork.toml` omits `[fixtures.*]`, `slipfork test` synthesizes `snap:migrate` once from `snow.yml`'s `pre-deploy` hooks (never running migrations twice).

### 2.2 Speed, Stability & Wire Contract

The 5 mandatory Speed & Stability Conditions and the `RunnerProtocol v1` NDJSON schema are specified **once**, normatively, in `ADR-0006` §2–§3. This page only documents the user-facing workflow.

---

## 3. Pure Tool Orthogonality: `$SLIPFORK_ARTIFACTS_DIR` + `[test].after_suite` (Coverage, Allure & Blastit)

Slipfork contains **zero** tool-specific flags, parsers, or collector traits; the normative `$SLIPFORK_ARTIFACTS_DIR` / `$SLIPFORK_RUN_ARTIFACTS_DIR` mount and `[test].after_suite` execution rules live in `ADR-0006` §4. User-facing recipes:

### Any Tool (Blastit, or your own report generator)
```yaml title="snow.yml"
modes:
  local:
    components:
      api:
        run_command: >-
          blastit dev --
          uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }}
```
The tool writes anywhere under `$SLIPFORK_ARTIFACTS_DIR`; its own merge/check command, if any, runs through `[test].after_suite` (see below). Slipfork has no Blastit-specific flags or traits.

### Python `coverage.py` Recipe
```yaml title="snow.yml"
modes:
  local:
    components:
      api:
        run_command: >-
          COVERAGE_FILE="$SLIPFORK_ARTIFACTS_DIR/.coverage.api"
          uv run coverage run --parallel-mode
          -m uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }}
```
```toml title="slipfork.toml"
[test]
after_suite = "uv run coverage combine \"$SLIPFORK_RUN_ARTIFACTS_DIR\"/*/.coverage* && uv run coverage report"
```

### Playwright / Allure Recipe
```toml title="slipfork.toml"
[test]
after_suite = "npx allure generate \"$SLIPFORK_RUN_ARTIFACTS_DIR\"/*/allure-results -o target/allure-report --clean"
```
