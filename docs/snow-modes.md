# `snow.yml` Modes & Secrets (`0.1.0`)

`modes:` is the one place environment differences live. `modes.all` carries the wiring every environment shares; each named mode adds, overrides, or removes the rest.

```console
$ slipfork snow resolve --file snow.yml --mode local
DATABASE_URL=postgres://127.0.0.1:5432/appdb
LOG_LEVEL=INFO
SECRET_KEY=[REDACTED]
```

```yaml title="snow.yml"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:8000
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
  cloud:
    envs:
      api:
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

!!! info "Commands are not templated"
    `run_command` is passed verbatim to `sh -eu -c`. Name the same literal port you declared in `ingresses` or `internal_ports`; keep `${{ ... }}` expressions in `envs` values.

## How a mode resolves

| Rule | Behaviour |
|---|---|
| `all` is the only base mode | `modes.all.envs.<group>` merges first; the named mode overlays it variable by variable |
| Groups, not components | `components[].envs: [api]` attaches group `api`; `modes.<mode>.envs.api` edits single variables in it |
| No top-level environment schema | Top-level `envs:` and `project:` are refused by key name; project identity belongs in `slipfork.toml` |
| Unknown modes fail closed | The error lists the concrete modes the manifest declares; `*` is not a mode |
| `up`/`fork` use `local` | A manifest without `modes.local` can resolve other modes but cannot boot a world |

## Components per mode

| You write | You get |
|---|---|
| `run_command: <cmd>` on a declared component | That mode resolves `<cmd>` instead of the top-level command |
| `run_command: <cmd>` on an undeclared component | A mode-only component; the command must be non-empty |
| `disabled: true` | The component and its environment groups disappear from that mode |

`modes.all.components` is rejected: universal components belong on top-level `components[]`. A mode-only addition cannot be disabled.

## `!!` secrets vs `??` fallbacks

| Operator | Syntax | When the key is missing | Allowed on |
|---|---|---|---|
| Fallback | `${{ vars::LOG_LEVEL ?? "INFO" }}` | Returns `"INFO"` for this render; writes nothing | Every namespace |
| Ensure | `${{ secrets::KEY !! random_string(50) }}` | Generates once, then persists it in the local vault | Single-key `secrets::<KEY>` only |

Generators are `random_string(1..=512)`, `random_hex(1..=256)`, `random_base64(1..=256)`, and `uuid4()`. A fallback must be a JSON string literal: `?? "INFO"` works, `?? INFO` fails.

## Where a value comes from

The local vault is `$XDG_DATA_HOME/slipfork/vaults/<project>.json` (default `~/.local/share/slipfork/vaults/`), mode `0600`. `--state-dir` does not move it, so set `$XDG_DATA_HOME` for a hermetic run.

The vault holds local values only: `secrets::`, `vars::`, and every local `!!`. CI-resolved secrets are never written there. They arrive HPKE-encrypted and live in `slipforkd` memory ([two tiers](reference/ospp.md#two-tiers)).

Any reference without `??` or `!!` that the vault cannot satisfy fails the whole render, listing every missing value in one pass:

```console
$ slipfork snow resolve --file snow.yml --mode local
slipfork: missing required values for mode 'local':
  - vars::MAP_STYLE (referenced by modes.local.envs.api.MAP_STYLE)
  - secrets::STRIPE_KEY (referenced by modes.local.envs.api.STRIPE_KEY)
```

## Coordinates and the single origin

A world has exactly one public origin, rendered by `slipforkd` when it materializes the world. `${{ resources::app.url }}` is that origin, and `${{ resources::app.domain }}` is its host.

- On port `80`, the URL is `http://<host>`; on an ephemeral port it is `http://<host>:<port>`.
- `ingresses[].target.port` is a container port and never appears in `resources::app.url`.
- A provider never supplies the origin, and it is never baked into a reusable snapshot.
- Local hosts follow `(<domain>.)?(<fork>.)?<project>.localhost[:<port>]`.
- `${{ domains::<ID> }}` has no provider in `0.1.0` and fails closed as a missing value.
- `components::<id>.private_port` comes from `ingresses`, else `internal_ports[0]`.

The full namespace and schema tables live in the [Manifest Reference](reference/manifest.md).
