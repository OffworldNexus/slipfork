# Manifest Reference (`0.1.0`)

Slipfork reads two files: `snow.yml` for the world and its services, and an optional `slipfork.toml` for project identity, the rootfs selector, and validated build/fixture intent.

```console
$ slipfork snow resolve --file snow.yml --mode local   # validate and render one mode
$ slipfork up                                           # boot with the `local` mode
```

## 1. `snow.yml`

```yaml title="snow.yml"
images:
  - id: api
    context: api
    dockerfile: Dockerfile # default

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:8000
    envs: [app]
    internal_ports: [8000]

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

hooks:
  - component: api
    when: pre-deploy # pre-deploy | post-deploy | failed-deploy
    run_command: uv run alembic upgrade head

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
    envs:
      app:
        DEBUG: 'True'
```

`run_command` is passed verbatim to `sh -eu -c`, so it is **not** templated. Name the same literal port you declared in `ingresses` or `internal_ports`; expressions belong in `envs` values.

`images[]` is either a build image (`id`, `context`, optional `dockerfile`, optional build `envs`) or a registry image (`id`, `registry`, `repository`, optional `tag`). A project needs at least one build image, because every world boots a built rootfs.

`domains: [{ id: admin }]` declares one local DNS label. Resource versions map to container images: `pg_clusters.version` → `postgres:<v>`, `redis_clusters.version` → `redis:<v>`, and a non-empty `s3_buckets` → SeaweedFS.

### Validation rules

- **Forbidden top-level keys fail closed.** `envs:` and `project:` are refused by key name; groups live under `modes`, project identity in `slipfork.toml`.
- **`*` is not a mode.** The universal base is `all`, and unknown keys fail closed in every table.
- **Cross-references are checked in one pass.** Every component image, ingress target, hook component, database cluster, and environment group must exist; all violations are reported together.
- **`modes.all.components` is rejected.** Universal components belong on top-level `components[]`.
- **`up` and `fork` hardcode `local`.** A manifest without `modes.local` can still resolve other modes but cannot boot a world.

### Components per mode

`components[]` plus `modes.<mode>.components.<id>` resolve through one function:

| Declaration | Effect in that mode |
|---|---|
| `run_command: <cmd>` on a declared id | Resolves `<cmd>` instead of the top-level command |
| `run_command: <cmd>` on an undeclared id | Adds the component; the command must be non-empty |
| `disabled: true` | Drops a top-level component and its environment groups |

A disabled component never resolves a command or environment, and `disabled: true` cannot target a mode-only addition. [`snow.yml` modes & secrets](../snow-modes.md) walks through the same rules.

### Expressions

```text
expr      := namespace "::" path ( ("??" json_string) | ("!!" generator) )?
generator := "random_string(" 1..=512 ")" | "random_hex(" 1..=256 ")"
           | "random_base64(" 1..=256 ")" | "uuid4()"
```

| Namespace | Reads | `??` | `!!` |
|---|---|---|---|
| `secrets::<KEY>` | Local `0600` vault `secrets` | yes | yes — `!!` is valid only here |
| `vars::<KEY>` | Local `0600` vault `vars` | yes | no |
| `domains::<ID>` | No provider in `0.1.0` — fails closed | yes | no |
| `resources::…`, `components::…`, `deployment::…` | World coordinates (below) | yes | no |

`!!` values persist in the project's local vault, which holds local values only. CI-resolved secrets are never written there ([two tiers](ospp.md#two-tiers)).

### Coordinates

`ResourceCoordinates::for_world` is the sole coordinate builder. It validates the world target against `domains[].id` and renders every URL from the world's single public origin, never from provider domains.

| Expression | Value |
|---|---|
| `resources::app.url` | The origin URL: portless at `:80`, `:<port>` otherwise |
| `resources::app.domain` | The origin host |
| `resources::pg_databases.<id>.url` | `postgres://127.0.0.1:5432/<id>` |
| `resources::redis_clusters.<id>.url` | `redis://127.0.0.1:6379/<index>` |
| `resources::s3_buckets.<id>.{endpoint_url,bucket,access_key,secret_key,region}` | World-local `http://127.0.0.1:9000` unless a remote plan supplies coordinates |
| `components::<id>.{private_url,private_domain,private_port}` | From `ingresses`, else `internal_ports[0]` |

`LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)` parses `(<domain>.)?(<fork>.)?<project>.localhost` and enforces the disjoint domain-label and fork-name rules.

## 2. `slipfork.toml`

```toml title="slipfork.toml"
# Optional ProjectSlug override; defaults to the checkout directory's basename.
project = "acme"

# Optional world-rootfs selector: two keys only, both inferred when unambiguous.
[world]
image = "api"     # snow.yml BuildImage id; mandatory iff snow.yml declares two or more
stage = "builder" # named `FROM ... AS <stage>`; keeps cargo/rustc in the world

[artifacts.front]
inputs = ["front/src/**", "front/package.json"]
build = "pnpm --dir front build"
mount = "front/dist"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "psql -f api/migrations/0001_schema.sql"

[test]
coverage = true
coverage_report = ["html", "xml"] # html | xml | json | lcov | text
coverage_fail_under = 85          # optional integer 0..=100
```

The whole file is optional: absent, `up` and `fork` boot with defaults. `artifacts` and `fixtures` are validated at boot, but `0.1.0` ships no command that materializes their plan. A malformed file always fails closed.

### `[world]`: which rootfs a world boots

| Key | Selects | Inferred when | Fails closed when |
|---|---|---|---|
| `image` | `snow.yml` build image id | Exactly one build image exists | None exists, or two or more exist and none is selected |
| `stage` | Named `FROM ... AS <stage>` | The final `FROM` is named | The stage is unnamed or unknown |

A project with two build images must set `world.image`; a Rust or Go project that needs the toolchain at runtime sets `world.stage = "builder"`.

```console
$ slipfork up
slipfork: snow.yml declares 2 buildable images ('api', 'worker') and slipfork.toml does not select one
  fix: add `world.image = "api"` under `[world]` in slipfork.toml
```

### Other validation rules

- **Unknown keys fail closed** in every `slipfork.toml` table.
- **`[resources.<kind>.<id>]`** takes `local` and `test` strategies (`local`, `prefer_remote`, `remote`); `slipforkd` applies the plan when it boots the world. `remote` fails closed without a live OSPP session.
- **`[test]` is typed fields only.** `coverage = true` is the sole switch, `coverage_report` defaults to `["text"]`, and `coverage_fail_under` must be `0..=100`. These fields validate; test execution is not part of `0.1.0`.
- **`[fixtures.<id>].per_world`** defaults to `false`. A per-world fixture is never reported `Cached` and cannot be the parent of an origin-independent fixture.

Back to the [CLI reference](cli.md) or [`snow.yml` modes & secrets](../snow-modes.md).
