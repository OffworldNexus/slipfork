# `snow.yml` Modes & Secrets (`0.1.0`)

`crates/slipfork-snow` and `slipfork snow resolve` implement `snow.yml` v2 parsing, structural validation, `modes` inheritance, and `!!` write-once CSPRNG secret generation.

---

## `modes` (`all`, `cloud`, `local`)

Applications share ~80% of their wiring (`DATABASE_URL`, `REDIS_URL`) across every environment while differing on the remaining 20% (cloud storage & Sentry vs local S3 endpoints, `DEBUG`, and reload commands):

```yaml title="snow.yml"
images:
  - id: api
    context: api

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:${{ components::api.private_port }}
    envs: [api]

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: psql, cluster: psql }]
  redis_clusters: [{ id: redis, version: "8" }]
  s3_buckets: [{ id: media }]

modes:
  all:
    envs:
      api:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.psql.url }}'
        REDIS_URL: '${{ resources::redis_clusters.redis.url }}'
  cloud:
    envs:
      api:
        AWS_STORAGE_BUCKET_NAME: '${{ vars::AWS_STORAGE_BUCKET_NAME }}'
        AWS_SECRET_ACCESS_KEY: '${{ secrets::AWS_SECRET_ACCESS_KEY }}'
  local:
    components:
      api:
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }} --reload
    envs:
      api:
        DEBUG: 'True'
        AWS_S3_ENDPOINT_URL: '${{ resources::s3_buckets.media.endpoint_url }}'
```

### Resolution Rules

1. **`modes` is the sole environment schema:** Top-level `envs:` is hard-rejected (`SnowError::TopLevelEnvsUnsupported`), as is top-level `project:` (see [Manifest Reference](reference/manifest.md#2-slipforktoml-slipfork_dagslipforkconfig) for `<vcs_root>/slipfork.toml` `ProjectSlug` rules).
2. **`all` is the sole universal base mode** (`*` fails with `SnowError::AsteriskModeForbidden`):
   - **Env groups:** `modes.all.envs.<G>` merges first, overlaid by `modes.<M>.envs.<G>`.
   - **Two-level `run_command` override (`SnowFile::effective_run_command` — M2 process execution):** `modes.<M>.components.<id>.run_command` overrides top-level `components[].run_command` (`modes.all.components` is rejected). In `0.1.0`, `run_command` overrides are parsed and structurally validated; component process supervision lands in **Milestone 2** (`crates/slipfork-edge`).

---

## Auto-Generated Secrets (`!!`) vs Fallbacks (`??`)

| Operator | Syntax | Behavior when key is missing | Allowed namespaces |
|---|---|---|---|
| **Fallback (`??`)** | `${{ vars::LOG_LEVEL ?? "INFO" }}` | Returns `"INFO"` transiently; **never writes** to any vault | `vars`, `secrets`, `domains` (`domains::<ID>` reads `OsppBundle.domains`, populated in **M2** `OSPP v1` cloud bundles) |
| **Ensure (`!!`)** | `${{ secrets::KEY !! random_string(50) }}` | Generates once via CSPRNG (`random_string(1..=512)`, `random_hex(1..=256)`, `random_base64(1..=256)`, `uuid4()`), **persists atomically** (`0600` vault), and reuses it thereafter | `secrets` only |

If any reference lacks `??` and `!!` and is absent from the active `SecretProvider`, `slipfork snow resolve` **fails closed** and reports **every** missing variable in a single pass:

```text
slipfork: missing required values for mode 'local':
  - vars::MAP_STYLE (referenced by modes.local.envs.api.MAP_STYLE)
  - secrets::STRIPE_KEY (referenced by modes.local.envs.api.STRIPE_KEY)
```

---

## `.localhost` Coordinates & Fork Disambiguation (`slipfork-snow` + `slipfork-world`)

`ResourceCoordinates::local_for_world(&snow, &ProjectSlug, &target)` is the sole local coordinate constructor: it validates `target` through `WorldTarget::validate(&snow.domain_ids())` (`slipfork_world::WorldError` wrapped via `SnowError::World`) and resolves `resources::app.url` to `http://(<fork>|test-<slot>.)?<project>.localhost`, while `LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)` is the canonical 1/2/3-label `.localhost` `Host`-header parser (`ADR-0005` §1).
