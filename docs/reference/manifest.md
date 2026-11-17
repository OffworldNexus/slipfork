# Manifest Reference (`0.1.0`)

Slipfork reads two files: `snow.yml` (`crates/slipfork-snow`) and `slipfork.toml` (`crates/slipfork-dag`).

## 1. `snow.yml` (`slipfork_snow::SnowFile`)

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
    components:
      api:
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port 8000 --reload
      worker: { disabled: true }
    envs:
      app:
        DEBUG: 'True'
```

`run_command` is passed verbatim to `sh -eu -c`, so it is **not** expression-templated: name the same literal port you declared in `ingresses`/`internal_ports` (here `8000`). `${{ ... }}` expressions belong in `envs` values, not in commands.

`images[]` is either a build image (`id`, `context`, optional `dockerfile` — default `Dockerfile` — and optional `envs` build groups) or a registry image (`id`, `registry`, `repository`, optional `tag` — default `latest`). Every world boots from a built `BuildImage` rootfs, so a project needs at least one build image (see [`[world]`](#2-slipforktoml-slipfork_dagslipforkconfig)).

`domains: [{ id: admin }]` declares one local DNS label. `resources:` map to backing-service containers: `pg_clusters.version` → `docker.io/library/postgres:<version>`, `redis_clusters.version` → `docker.io/library/redis:<version>`, and a non-empty `s3_buckets` → SeaweedFS. `slipfork db` and `slipfork redis` open `psql`/`redis-cli` inside those containers.

### Validation rules

- **Forbidden top-level keys fail closed.** `envs:` and `project:` raise `SnowError::ForbiddenTopLevelKey { key }`, naming the key and never its contents. Environment groups live under `modes.all` / `modes.<mode>`; project identity lives in `slipfork.toml` (§2).
- **`*` is not a mode** (`SnowError::AsteriskModeForbidden`); the universal base is `all`, and unknown keys fail closed in every table (`#[serde(deny_unknown_fields)]`). `slipfork snow resolve --mode <mode>` renders any declared mode, but `up`/`fork` hardcode `local`, so a manifest without `modes.local` fails closed.
- **Cross-references are checked in one pass.** Every `components[].image`, `ingresses[].target.component`, `hooks[].component`, `pg_databases[].cluster`, and `envs` group name must exist; all violations are reported together in `SnowError::Validation`. `modes.all.components` is rejected — universal defaults belong on top-level `components[]`.

### Mode-scoped components (`SnowFile::effective_components`)

`components[]` plus `modes.<mode>.components.<id>` resolve through one function — nothing else reads mode component overrides:

| Declaration | Effect in that mode |
|---|---|
| `run_command: <cmd>` on a declared id | resolves `<cmd>` instead of `components[].run_command` |
| `run_command: <cmd>` on an undeclared id | adds the component (non-empty `run_command` required) |
| `disabled: true` | drops a top-level component (and its env groups) from the mode |

A disabled component never resolves a command or environment, and `disabled: true` cannot target a mode-only addition. [`snow.yml` modes & secrets](../snow-modes.md) walks through the same rules with a task-oriented example.

### Expressions (`${{ ... }}`)

```text
expr      := namespace "::" path ( ("??" json_string) | ("!!" generator) )?
generator := "random_string(" 1..=512 ")" | "random_hex(" 1..=256 ")"
           | "random_base64(" 1..=256 ")" | "uuid4()"
```

| Namespace | Reads | `??` | `!!` |
|---|---|---|---|
| `secrets::<KEY>` | local vault `secrets` | yes | yes — `!!` is valid only here |
| `vars::<KEY>` | local vault `vars` | yes | no |
| `domains::<ID>` | no provider today — fails closed | yes | no |
| `resources::…`, `components::…`, `deployment::…` | world coordinates (below) | yes | no |

The local vault is `$XDG_DATA_HOME/slipfork/vaults/<project>.json` (default `~/.local/share/slipfork/vaults/`) mode `0600`, and it is layer 1 of `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>`: the only `!!` write sink. A session paired by `slipfork auth login` supplies `OSPP v1` `env.bundle` keys second, so an unpaired developer sees a key the vault lacks as a missing value ([modes & secrets](../snow-modes.md)).

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

`LocalHostRoute::parse(&project, &snow.domain_ids(), host_header)` parses `(<domain>.)?(<fork>.)?<project>.localhost` and enforces the disjoint `domains[].id` / fork rules (`ADR-0005` §1).

---

## 2. `slipfork.toml` (`slipfork_dag::SlipforkConfig`)

```toml title="slipfork.toml"
# Optional canonical ProjectSlug override; defaults to the directory basename of the VCS
# checkout, resolved through the Git store (a colocated Jujutsu checkout included).
project = "acme"

# Optional world-rootfs selector: two keys only, both inferred when unambiguous.
[world]
image = "api"    # snow.yml BuildImage id; mandatory iff snow.yml declares >= 2
stage = "builder" # named `FROM ... AS <stage>`; a compiled stage keeps cargo/rustc in the world

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

The whole file is optional: absent, `up`/`fork` boot with defaults. `dag plan` reads it explicitly (`--file`, default `<vcs_root>/slipfork.toml`), so a repository using artifacts or fixtures declares them there. A malformed file always fails closed.

### `[world]`: which rootfs a world boots

Every world's read-only rootfs is built from one `snow.yml` `BuildImage`, so `[world]` selects it and its Dockerfile stage. Both keys are optional and inferred when only one choice exists; `[world]` rejects any other key.

| Key | Selects | Inferred when | Fails closed when |
|---|---|---|---|
| `image` | `snow.yml` `images[].id` used for the rootfs | exactly one `BuildImage` exists | no `BuildImage` exists, or `>= 2` exist and none is selected |
| `stage` | named `FROM ... AS <stage>` inside that Dockerfile | the final `FROM` is named | the stage is unnamed or unknown |

```console
$ slipfork up
slipfork: snow.yml declares 2 buildable images ('api', 'worker') and slipfork.toml does not select one
  fix: add `world.image = "api"` under `[world]` in slipfork.toml
```

A project with two build images (say `api` and `worker`) must set `world.image`; a Rust/Go project that needs `cargo`/`rustc` at runtime sets `world.stage = "builder"`. A project with no build image fails closed with the 4-line `images:` snippet to add: it cannot be built into an image, so Slipfork cannot fork it.

### Validation rules

- **Unknown keys fail closed** in every `slipfork.toml` table (`#[serde(deny_unknown_fields)]`).
- **`[resources.<kind>.<id>]`** (`pg_databases` / `s3_buckets`) takes `local` and `test` strategies (`local` | `prefer_remote` | `remote`), e.g. `[resources.pg_databases.appdb] local = "prefer_remote"`; `slipforkd` applies the plan when it boots the world (`ADR-0008` §3). `prefer_remote` degrades to world-local coordinates with a `not_offered` row; `remote` without a paired OSPP session fails closed naming the missing offer. Seed dumps, `--no-dump`, and `--refresh-dump`: [CLI reference](cli.md#seed-dumps-and-sealed-bases).
- **`[test]` is typed fields only** (`ADR-0006` §1): `coverage = true` is the sole switch, `coverage_report` defaults to `["text"]`, options without it fail with `DagError::CoverageOptionsRequireEnabled`, `coverage_report = []` with `DagError::EmptyCoverageReport`, and `coverage_fail_under` outside `0..=100` with `DagError::InvalidCoverageThreshold`. Coverage execution is not shipped in `0.1.0`; it lands with `STP v1` in `0.2.0`.
- **`[fixtures.<id>].per_world`** defaults to `false`. A per-world fixture is never reported `CacheAction::Cached` and cannot be the `parent` of an origin-independent fixture (`DagError::OriginIndependentChildOfPerWorld`). Per-world snapshot execution is not shipped in `0.1.0`; it lands with `STP v1` in `0.2.0`.
