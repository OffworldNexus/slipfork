# `snow.yml` Modes & Secrets (`0.1.0`)

`modes:` is the one place environment differences live. `modes.all` carries the wiring every environment shares; each named mode adds, overrides, or removes the rest. `slipfork snow resolve --mode <mode>` renders the result and persists `!!` secrets.

```yaml title="snow.yml"
images:
  - id: api
    context: api
  - id: mailpit
    registry: docker.io
    repository: axllent/mailpit
    tag: v1.20

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:8000
    envs: [api]
  - id: mailpit
    image: mailpit
    run_command: mailpit --smtp 0.0.0.0:1025
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
    components:
      mailpit: { disabled: true }
    envs:
      api:
        AWS_STORAGE_BUCKET_NAME: '${{ vars::AWS_STORAGE_BUCKET_NAME }}'
        AWS_SECRET_ACCESS_KEY: '${{ secrets::AWS_SECRET_ACCESS_KEY }}'
  local:
    components:
      api:
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port 8000 --reload
    envs:
      api:
        DEBUG: 'True'
        AWS_S3_ENDPOINT_URL: '${{ resources::s3_buckets.media.endpoint_url }}'
```

`run_command` is passed verbatim to `sh -eu -c`, so it is **not** expression-templated: name the same literal port you declared in `ingresses`/`internal_ports` (here `8000`). `${{ ... }}` expressions belong in `envs` values, not in commands.

---

## How a Mode Resolves

| Rule | Behaviour |
|---|---|
| **`all` is the only base mode** | `modes.all.envs.<group>` merges first; `modes.<mode>.envs.<group>` overlays it variable by variable. `modes.*` fails with `SnowError::AsteriskModeForbidden`. |
| **Environment groups are keyed by group, not by component** | `components[].envs: [api]` attaches group `api`; `modes.<mode>.envs.api` adds or overrides single variables inside it. |
| **No top-level environment schema** | Top-level `envs:` and `project:` fail with `SnowError::ForbiddenTopLevelKey { key }`, which names the key only — never its contents. |
| **Unknown modes fail closed** | `SnowError::UnknownMode { requested, available }` lists the concrete modes the manifest declares (`all` is universal, so it is not offered as a choice). |

## Adding, Overriding, or Removing Components per Mode

`modes.<mode>.components.<id>` is resolved together with top-level `components[]` by `SnowFile::effective_components(mode)`:

| You write | You get |
|---|---|
| `run_command: <cmd>` on a declared component | that mode resolves `<cmd>`; `slipforkd` starts it |
| `run_command: <cmd>` on an undeclared component | a mode-only component (must be non-empty) |
| `disabled: true` | the component is gone from that mode, including its environment groups |

- `modes.all.components` is rejected: universal components belong on top-level `components[]`.
- `disabled: true` only removes top-level components; a mode-only addition cannot be disabled.
- A disabled component fails closed when asked to resolve a command or environment.

## Auto-Generated Secrets (`!!`) vs Fallbacks (`??`)

| Operator | Syntax | When the key is missing | Allowed on |
|---|---|---|---|
| **Fallback** | `${{ vars::LOG_LEVEL ?? "INFO" }}` | returns `"INFO"` for this render; **writes nothing** | every namespace |
| **Ensure** | `${{ secrets::KEY !! random_string(50) }}` | generates once and persists it in the local vault | single-key `secrets::<KEY>` only |

The generator grammar (`random_string`, `random_hex`, `random_base64`, `uuid4`) lives in the [manifest reference](reference/manifest.md). A `??` fallback must be a JSON string literal, so `${{ vars::LOG_LEVEL ?? INFO }}` fails while `?? "INFO"` works.

### Where a key is read from

The local vault is `$XDG_DATA_HOME/slipfork/vaults/<project>.json` (default `~/.local/share/slipfork/vaults/`), mode `0600`; `--state-dir`/`--runtime-dir` do not move it, so set `$XDG_DATA_HOME` for a hermetic run. `secrets::` and `vars::` resolve there. Remote provider protocol: [OSPP v1](reference/ospp.md).

Any reference without `??` or `!!` that the vault cannot satisfy fails the whole render, listing **every** missing value in one pass:

```console
$ slipfork snow resolve --file snow.yml --mode local
slipfork: missing required values for mode 'local':
  - vars::MAP_STYLE (referenced by modes.local.envs.api.MAP_STYLE)
  - secrets::STRIPE_KEY (referenced by modes.local.envs.api.STRIPE_KEY)
```

The command exits `1` and prints nothing on stdout.

---

## Coordinates & the Single World Origin

`ResourceCoordinates::for_world` is the sole coordinate builder, and a world has **one** public origin:

- `${{ resources::app.url }}` is that origin's URL — `http://<host>` when the world origin is on port `80`, `http://<host>:<port>` otherwise, as `slipforkd` renders it when it binds an ephemeral port. `ingresses[].target.port` is a container port and never appears in `app.url`. `${{ resources::app.domain }}` is the origin host.
- Slipfork assigns the origin when it materializes the world; a provider never supplies it, and it is never baked into a reusable snapshot (`ADR-0005` §1).
- `slipfork snow resolve` renders canonical-world coordinates, so `resources::app.url` is `http://<project>.localhost` today.
- Local hosts follow `(<domain>.)?(<fork>.)?<project>.localhost[:<port>]`, parsed by `LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)`. `domains[].id` and fork names are disjoint single lowercase DNS labels, and fork names never start with `test-`.
- `${{ domains::<ID> }}` has no provider in `0.1.0`; it fails closed as a missing value. The single-origin edge derives local `domains::<id>` values itself (`ADR-0005` §1).
- `${{ components::<id>.private_port }}` comes from `ingresses`, else `internal_ports[0]`; a component that declares neither fails with `SnowError::UnknownResourcePath`.

The full namespace and schema tables live in the [Manifest Reference](reference/manifest.md).
