# Manifest Reference (`0.1.0`)

Exact `snow.yml` v2 (`crates/slipfork-snow`) and `slipfork.toml` (`crates/slipfork-dag`) schemas implemented in `0.1.0`.

---

## 1. `snow.yml` v2 (`slipfork_snow::SnowFile`)

```yaml title="snow.yml"
images:
  - id: api
    context: api
    dockerfile: Dockerfile
    envs: []

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:${{ components::api.private_port }}
    envs: [app]
    internal_ports: [8000]

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

hooks:
  - component: api
    when: pre-deploy # pre-deploy | post-deploy | failed-deploy
    run_command: uv run alembic upgrade head

domains:
  - id: admin # Single lowercase DNS label without '.'

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: appdb, cluster: psql, pool: true }]
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
    envs:
      app:
        DEBUG: 'True'
```

### Validation & Template Rules

- **Top-level `envs` and `project` rejected:** `SnowFile::parse_yaml` rejects top-level `envs:` (`TopLevelEnvsUnsupported`), `*` mode keys (`AsteriskModeForbidden`), top-level `project:` (`Validation` — project identity lives in `<vcs_root>/slipfork.toml`; see §2), and unknown fields (`#[serde(deny_unknown_fields)]`).
- **Two-level `run_command` override (`SnowFile::effective_run_command` — M2 process execution):** `modes.<M>.components.<id>.run_command` overrides `components[].run_command`; `modes.all.components` and unknown component IDs fail validation in `0.1.0`. Component process supervision is **Milestone 2** (`crates/slipfork-edge`) — `0.1.0` `slipfork snow resolve` validates the manifest and resolves `envs` only.
- **`Faceless` v2 expressions (`${{ ... }}`):**
  ```text
  expr       := namespace "::" path ( ("??" json_string) | ("!!" generator) )?
  generator  := "random_string(" 1..=512 ")" | "random_hex(" 1..=256 ")" | "random_base64(" 1..=256 ")" | "uuid4()"
  ```
  - Provider namespaces: `secrets::<KEY>`, `vars::<KEY>`, `domains::<ID>` (`!!` is valid **only** on single-key `secrets::<KEY>`). Note: `domains::<ID>` reads `OsppBundle.domains`, which is populated by remote `OSPP v1` cloud bundles in **Milestone 2** (`0.1.0` `slipfork snow resolve` does not auto-populate `OsppBundle.domains` from top-level `domains:`).
  - Coordinate namespaces (`ResourceCoordinates::local_for_world`):
    - `resources::app.{url, domain}` (`http://(<fork>|test-<slot>.)?<project>.localhost`)
    - `resources::pg_databases.<id>.url`, `resources::redis_clusters.<id>.url`
    - `resources::s3_buckets.<id>.{endpoint_url, bucket, access_key, secret_key, region}`
    - `components::<id>.{private_url, private_domain, private_port}` (resolved from `ingresses` or `internal_ports`; fails closed with `UnknownResourcePath` if the component declares neither)
    - `deployment::env.name`, `deployment::is_production`
- **`.localhost` route disambiguation (`slipfork_world::LocalHostRoute`):** `LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)` is the canonical entry point that parses `(<domain>.)?(<fork>.)?<project>.localhost` and enforces disjoint `WorldTarget` / `domains[].id` rules (`ADR-0005` §1).

---

## 2. `slipfork.toml` (`slipfork_dag::SlipforkConfig`)

```toml title="slipfork.toml"
# Optional canonical ProjectSlug override for http://<project>.localhost and the local vault name.
# Defaults to <vcs_root>'s directory basename in standard Git repos (including linked `.worktrees/<fork>`,
# never probing worktree-local files) and via `jj root` when `jj` is on PATH. Required for Git submodules,
# `--separate-git-dir`, or when `jj` is absent from PATH; in `.worktrees/<fork>` under `jj`, set it,
# otherwise the fork/workspace name is used until M2's JjWorkspaceAdapter lands (ADR-0005 §3).
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
```

---

## 3. Vault Bundle Format (`slipfork_snow::OsppBundle`)

Local `0600` vaults (`FileVaultProvider`) and static `OsppBundle` snapshots serialize as (`0.1.0` `slipfork snow resolve` writes `!!`-generated `secrets`; remote `domains` bundle loading arrives in **Milestone 2** `OSPP v1`):

```json
{
  "vars": { "PUBLIC_MAP_STYLE": "positron" },
  "secrets": { "SECRET_KEY": "9fK2..." },
  "domains": {}
}
```
