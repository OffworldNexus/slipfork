# Slipfork M2–M4 Target Developer Experience Specification (`docs/design/target-dx.md`)

- **Status:** Accepted Target DX Specification (`ADR-0001`..`ADR-0007`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here are promoted into published `docs/` and `README.md` **only** as each M2–M4 binary capability ships and passes conformance tests.

---

## 1. Milestone 2 Target DX: Single-Origin Worlds, `slipforkd`, `weaved` Leases & VCS Workspaces

### 1.1 Install & One-Time Port-80 Edge Grant

Installing the workspace places `slipfork`, `slipforkd`, and `git-remote-slipfork` onto `PATH`:

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork
```

`slipforkd` binds loopback port `80` (`127.0.0.1:80` / `[::1]:80`) via systemd socket activation (`slipforkd.socket`) or `/etc/sysctl.d/90-slipfork-port80.conf` (`net.ipv4.ip_unprivileged_port_start=80`), failing closed in `slipfork doctor` and `slipfork up` if unavailable. **OSPP is not required to run M2:** local worlds boot out of the box from `~/.local/share/slipfork/vaults/<project>.json` (`0600`) and derived `.localhost` origins (`ADR-0005` §1).

### 1.2 `slipfork up`, Single-Origin World Cards & `.localhost` Routing

```console
$ slipfork up
[secrets] generated secrets::SECRET_KEY -> local vault (~/.local/share/slipfork/vaults/acme.json)
[world]   psql:18 ready, redis:8 ready, seaweedfs:s3 ready (isolated netns)
[hook]    pre-deploy (api-migrate): uv run python manage.py migrate (410 ms)
[origin]  http://acme.localhost (uid=1000:1000, git remote 'slipfork' configured)
```

| Local URL (`WorldOrigin::Local`) | Routed Target (`LocalHostRoute`) |
|---|---|
| `http://acme.localhost` | Canonical world (`WorldTarget::Canonical`), default ingress |
| `http://admin.acme.localhost` | Canonical world, `snow.yml` `domains: [{ id: admin }]` |
| `http://feat-auth.acme.localhost` | Fork `feat-auth` (`WorldTarget::UserFork`), default ingress |
| `http://admin.feat-auth.acme.localhost` | Fork `feat-auth`, derived local domain `admin` |

- **Single Canonical Origin & Precedence:** Every world has **at most one public origin** (`WorldOrigin = Local { url } | Shared { url }`), assigned at world materialization and supplied by Slipfork at boot (`modes.local` carries no hardcoded `BASE_URL` literal; local `domains::<id>` derives from `(project, target, id)` and always overrides provider domains — see `ADR-0005` §1).
- **Mode-Scoped Components (`modes.<mode>.components.<id>`):** `snow.yml` supports mode-scoped component overrides (`run_command` override, local-only dev watchers/sandboxes, or disabling cloud-only workers per `ADR-0001` §1); `run_command` executes via `sh -eu -c` under clean `ExecEnv`.

### 1.3 Interactive Shells, `git-remote-slipfork` (`<fork>` + `<fork>/wip`) & Jujutsu (`jj workspace`)

```console
$ slipfork fork feat-auth               # Creates .worktrees/feat-auth (git worktree or jj workspace) + CoW world
$ slipfork shell feat-auth              # Enters interactive $SHELL directly inside feat-auth (no cd needed)

# From the canonical checkout:
$ git pull slipfork feat-auth           # Pull committed branch tip via git-remote-slipfork
$ git fetch slipfork feat-auth/wip      # Fetch synthesized commit of uncommitted + untracked edits
$ git diff HEAD..FETCH_HEAD
```

- Git `<fork>/wip` scratch-index synthesis and Jujutsu `JjWorkspaceAdapter` mechanics live in `ADR-0005` §3.

### 1.4 Public Sharing via Tunnel Weaver (`weaved`) — Materialization-Time Single Origin

```console
$ slipfork share feat-auth
# Illustrative public URL only — weaved's hostname format & wire spec are owned by tunnel-weaver (H-006):
[world]  re-materialized feat-auth from snapshot -> https://<illustrative-weaved-host>
$ slipfork list
WORLD       ORIGIN
canonical   http://acme.localhost
feat-auth   https://<illustrative-weaved-host>
$ slipfork share --stop feat-auth
[world]  re-materialized feat-auth from snapshot -> http://feat-auth.acme.localhost
```

- `slipforkd` is the sole ingress for both `.localhost` and shared hostnames (`H-008`): sharing tears down and re-creates the world from its snapshot with the single new `WorldOrigin` (minted ahead of boot against `slipforkd`'s planned static port), prints **exactly one URL per world**, and answers `http://*.acme.localhost` requests targeting a shared world (including `snow_domain` ingresses) with `308 Permanent Redirect` (`421` on WebSocket/Upgrade; `ADR-0005` §1–§2). Concrete `WeavedRegistrar` wiring remains gated on `tunnel-weaver` publishing `weaved`'s IPC spec (`H-006`).

### 1.5 Planned M2–M3 CLI Additions

| Planned Command | Milestone | Target Behavior |
|---|---|---|
| `slipfork up` / `slipfork down` | M2 | Start/stop canonical local world & register routes in `slipforkd` |
| `slipfork shell [<fork>]` | M2 | Interactive `$SHELL` inside `<fork>` (or `cwd`'s bound world) |
| `slipfork share [--stop] [<fork>]` | M2 | Re-materialize world with/without a `weaved` tunnel origin (`H-006`) |
| `slipfork list` / `slipfork prune` | M2 | Print active worlds (one canonical URL per world); GC unreferenced state |
| `slipfork secret set <KEY>` | M2 | Write secret from `stdin` into `0600` local vault |
| `slipfork auth login <url> --project <slug>` | M2 | Optional `OSPP v1` login for team/cloud secret parity |
| `slipfork support [--json]` | M2/M3 | Inspect verified integration tiers & evidence (`support/integrations.toml`, `ADR-0007`) |
| `slipfork test [-j N] [-- <runner...>]` | M3 | Materialize DAG, run tests across warm CoW worlds, collect typed coverage |

---

## 2. Milestone 3 Target DX: World-Owns-The-Stack Testing & Per-World Fixtures (`slipfork test`)

### 2.1 World-Owns-The-Stack Inversion & Verified Support Tiers

`snow.yml` + `slipfork.toml` boot the migrated, seeded, routed stack before tests run, injecting Slipfork-resolved `BASE_URL=http://test-<slot>.<project>.localhost` plus `DATABASE_URL`, `REDIS_URL`, and `AWS_S3_ENDPOINT_URL` (`ADR-0006` §2–§4). Integration maturity for each target stack (Flask/FastAPI, Node/TS Playwright/Vitest, PHPUnit, Go, Rust `nextest`, and Django `pytest-slipfork` Adapter #1) is reported strictly from `support/integrations.toml` (`none`..`hardened`, `ADR-0007`).

### 2.2 Reusable vs `per_world = true` Fixtures (`slipfork.toml`)

```toml title="slipfork.toml"
project = "acme"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "uv run python manage.py migrate"

[fixtures.seed_catalog]
parent = "migrate"
inputs = ["api/fixtures/catalog.json"]
run = "uv run python manage.py loaddata catalog" # Origin-independent -> reusable CoW snapshot

[fixtures.seed_sites]
parent = "seed_catalog"
per_world = true                                  # Writes BASE_URL into DB -> runs at boot per world, never shared
inputs = ["api/fixtures/configure_site.py"]
run = "uv run python api/fixtures/configure_site.py"
```

- When `slipfork.toml` omits `[fixtures.*]`, `slipfork test` synthesizes `snap:migrate` once from `snow.yml`'s `pre-deploy` hooks. Normative `per_world` rules live in `ADR-0003` §1 and `ADR-0005` §1.

---

## 3. Built-In Typed Coverage & Tool-Owned `SLIPFORK_OUTPUT_DIR` (`ADR-0006` §5)

Slipfork manifests contain **zero shell pipelines, manual path assembly, or tool-path interpolation** (`ADR-0006` §1):

### 3.1 Multi-World Coverage in 3 Lines (`[test]`)

```toml title="slipfork.toml"
[test]
coverage = true
coverage_report = ["html", "xml"]
coverage_fail_under = 85        # optional
```

The active `TestAdapter` (`CoverageDriver`) injects per-world coverage env (`COVERAGE_FILE`, `NODE_V8_COVERAGE`, PHP clover path, or `LLVM_PROFILE_FILE`) into both `snow.yml` world services and test shards, then combines all `N` world shards in `TestCoordinator` without user shell scripts.

### 3.2 Blastit, Playwright Traces & Allure (`SLIPFORK_OUTPUT_DIR`)

```yaml title="snow.yml"
modes:
  local:
    components:
      api:
        run_command: >-
          blastit dev --
          uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }}
```

Slipfork passes each world and test runner a documented, surviving directory in `SLIPFORK_OUTPUT_DIR` (`.slipfork/runs/<run_id>/<slot>/output`); Blastit, Playwright traces, and Allure write there directly through their own tooling and survive world teardown.
