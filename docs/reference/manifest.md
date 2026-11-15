# Manifest Reference (`0.1.0`)

The two files Slipfork reads: `snow.yml` (`crates/slipfork-snow`) and `slipfork.toml` (`crates/slipfork-dag`).

## 1. `snow.yml` (`slipfork_snow::SnowFile`)

```yaml title="snow.yml"
images:
  - id: api
    context: api
    dockerfile: Dockerfile # default

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:${{ components::api.private_port }}
    envs: [app]
    internal_ports: [8000]
  - id: worker
    image: api
    run_command: celery -A app worker
    envs: [app]

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

hooks:
  - component: api
    when: pre-deploy # pre-deploy | post-deploy | failed-deploy
    run_command: uv run alembic upgrade head

domains:
  - id: admin # single lowercase DNS label, no '.'

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: appdb, cluster: psql }]
  redis_clusters: [{ id: cache, version: "8" }]
  s3_buckets: [{ id: media }]

modes:
  all:
    envs:
      app:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.appdb.url }}'
  local:
    components:
      api:
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }} --reload
      worker: { disabled: true }
    envs:
      app:
        DEBUG: 'True'
```

`images[]` is either a build image (`id`, `context`, optional `dockerfile` — default `Dockerfile` — and optional `envs` build groups) or a prebuilt image (`id`, `registry`, `repository`, optional `tag` — default `latest`).

### Validation rules

- **Forbidden top-level keys fail closed.** `envs:` and `project:` raise `SnowError::ForbiddenTopLevelKey { key }`, which names the key and never echoes what it contained: `slipfork: top-level 'envs' is not supported in snow.yml; environment groups live under 'modes.all' / 'modes.<mode>' and project identity lives in slipfork.toml`. Environment groups live under `modes.all` / `modes.<mode>`; project identity lives in `<vcs_root>/slipfork.toml` (§2).
- **`*` is not a mode.** `modes.*` fails with `SnowError::AsteriskModeForbidden`; the universal base mode is `all`.
- **Unknown keys fail closed** in every table (`#[serde(deny_unknown_fields)]`).
- **Cross-references are checked in one pass.** Every `components[].image`, `ingresses[].target.component`, `hooks[].component`, `pg_databases[].cluster`, and `envs` group name must exist, and `domains[].id` must be a single unreserved lowercase DNS label; all violations are reported together in `SnowError::Validation`.
- **`modes.all.components` is rejected.** Universal defaults belong on top-level `components[]`.

### Mode-scoped components (`SnowFile::effective_components`)

`components[]` plus `modes.<mode>.components.<id>` resolve through one function — nothing else reads mode component overrides:

| Declaration | Effect in that mode |
|---|---|
| `run_command: <cmd>` on a declared id | resolves `<cmd>` instead of `components[].run_command` (supervision lands with `slipforkd` in `0.2.0`) |
| `run_command: <cmd>` on an undeclared id | adds the component (non-empty `run_command` required) |
| `disabled: true` | drops a top-level component (and its env groups) from the mode |

A disabled component never resolves a command or an environment, and `disabled: true` cannot target a mode-only addition. [`snow.yml` modes & secrets](../snow-modes.md) walks through the same rules with a task-oriented example.

### Expressions (`${{ ... }}`)

```text
expr      := namespace "::" path ( ("??" json_string) | ("!!" generator) )?
generator := "random_string(" 1..=512 ")" | "random_hex(" 1..=256 ")"
           | "random_base64(" 1..=256 ")" | "uuid4()"
```

| Namespace | Reads | `??` | `!!` |
|---|---|---|---|
| `secrets::<KEY>` | local vault `secrets`, then remote provider | yes | yes — `!!` is valid only here |
| `vars::<KEY>` | local vault `vars`, then remote provider | yes | no |
| `domains::<ID>` | provider `domains` | yes | no |
| `resources::…`, `components::…`, `deployment::…` | world coordinates (below) | yes | no |

The local vault is `$XDG_DATA_HOME/slipfork/vaults/<project>.json` (default `~/.local/share/slipfork/vaults/`) mode `0600`; today it is the only provider `slipfork snow resolve` reads. Remote `vars`/`secrets`/`domains` arrive over the `OSPP v1` WebSocket `EnvBundle` (`ADR-0008` §2), wired by `slipforkd` in `0.2.0`.

### Coordinates (`ResourceCoordinates::for_world`)

`for_world(&snow, &project, &target, &origin, &s3_overrides)` is the sole coordinate builder: it validates `target` against `domains[].id` and renders every URL from the world's single public `WorldOrigin` (`ADR-0005` §1) — never from provider `domains`.

| Expression | Value |
|---|---|
| `resources::app.url` | `origin.url()` — portless at `:80`, `:<port>` otherwise |
| `resources::app.domain` | `origin.host()` |
| `resources::pg_databases.<id>.url` | `postgres://127.0.0.1:5432/<id>` |
| `resources::redis_clusters.<id>.url` | `redis://127.0.0.1:6379/<index>` |
| `resources::s3_buckets.<id>.{endpoint_url,bucket,access_key,secret_key,region}` | world-local `http://127.0.0.1:9000` unless a resource plan supplies remote coordinates |
| `components::<id>.{private_url,private_domain,private_port}` | from `ingresses`, else `internal_ports[0]`; `UnknownResourcePath` when neither is declared |
| `deployment::env.name`, `deployment::is_production` | `local`, `false` |

`LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)` parses `(<domain>.)?(<fork>.)?<project>.localhost` and enforces the disjoint `domains[].id` / fork rules (`ADR-0005` §1).

---

## 2. `slipfork.toml` (`slipfork_dag::SlipforkConfig`)

```toml title="slipfork.toml"
# Optional canonical ProjectSlug override for http://<project>.localhost and the local vault
# name. Defaults to <vcs_root>'s directory basename in standard Git repos (including linked
# .worktrees/<fork>, never probing worktree-local files) and via `jj root` when `jj` is on
# PATH. Required for Git submodules, `--separate-git-dir`, or when `jj` is absent from PATH.
project = "acme"

[artifacts.front]
inputs = ["front/src/**", "front/package.json"]
build = "pnpm --dir front build"
mount = "front/dist"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "psql -f api/migrations/0001_schema.sql"

[fixtures.seed_catalog]
parent = "migrate"
inputs = ["api/migrations/seed_catalog.sql"]
run = "psql -f api/migrations/seed_catalog.sql"

# Writes absolute origin URLs into persistent state; never shared across worlds (ADR-0003 §1).
[fixtures.seed_sites]
parent = "seed_catalog"
per_world = true
run = "uv run python manage.py seed_sites"

[test]
coverage = true
coverage_report = ["html", "xml"] # html | xml | json | lcov | text
coverage_fail_under = 85          # optional integer 0..=100
```

### Validation rules

- **Unknown keys fail closed** in every `slipfork.toml` table (`#[serde(deny_unknown_fields)]`).
- **`[resources.<kind>.<id>]`** (`pg_databases` / `s3_buckets`, strategies `local` | `prefer_remote` | `remote`) parses and validates today; the plan that consumes it is wired by `slipforkd` in `0.2.0` (`ADR-0008` §3).
- **`[test]` is typed fields only** — no hooks, shell pipelines, or path assembly (`ADR-0006` §1). `coverage = true` is the sole switch; `coverage_report` defaults to `["text"]`; either coverage option without `coverage = true` fails with `DagError::CoverageOptionsRequireEnabled`; an explicitly empty `coverage_report = []` fails with `DagError::EmptyCoverageReport`; `coverage_fail_under` must be an integer in `0..=100` (`DagError::InvalidCoverageThreshold`). Coverage execution itself lands in Milestone 3 (`crates/slipfork-test`).
- **`[fixtures.<id>].per_world`** defaults to `false`. Fixture snapshots are shareable across worlds unless `per_world = true` marks a step that bakes this world's absolute origin URLs into persistent state (`ADR-0003` §1). A per-world fixture is never reported `CacheAction::Cached` and cannot be the `parent` of an origin-independent fixture (`DagError::OriginIndependentChildOfPerWorld`). Per-world snapshot execution lands in Milestone 3.
