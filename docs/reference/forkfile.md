# `Forkfile` Reference

Every project has one `Forkfile` at the repository root. It is parsed, checked as a whole file, then evaluated against a feature selection before `slipforkd` boots anything.

```text
project demo
features { default = [dev]; dev; }
postgres pg { database app_db }
service api ./api { port 8000; route /api; run ./app }
task migrate in api: ./manage migrate
```

!!! info "Statements are newline-separated"
    `;` is accepted between statements on one line. Blocks use `{ ... }`, and an `on` condition takes one statement after `:` — `on dev: task migrate in api: ./manage migrate`.

## Top-level statements

| Statement | Purpose |
|---|---|
| `project <slug>` | Overrides the VCS-derived project slug used in URLs and the vault path |
| `features { ... }` | Declares features and their transitive implications |
| `postgres <cluster> { ... }` | A PostgreSQL cluster and its databases |
| `redis <id> [ { version 7 } ]` | A Redis cluster |
| `s3 <id>` | An S3 bucket served by the world's object store |
| `domain <id>` | A DNS label on the world origin |
| `env <group> { ... }` | A reusable environment group |
| `service <id> <./dir \| from <image>> { ... }` | One container and its supervision |
| `on <expr> { ... }` / `on <expr>: <stmt>` | Conditional block at top level or inside `service`, `postgres`, `redis` |
| `task <name> in <service>: <argv...>` | A named command run by `slipfork run <name>` |

## Services

`service <id> ./dir` sets the Docker build context to `<dir>`, builds `<dir>/Dockerfile`, and runs with `<worktree>/<dir>` as the working directory. `service <id> from <registry/repo:tag>` pulls an OCI image and rejects build-only directives.

| Directive | Value | Notes |
|---|---|---|
| `run` | `<argv...>` | Exactly one supervised process, executed directly |
| `port` | `1..=65535` | The routed public port |
| `internal_ports` | `[<u16>, ...]` | Unrouted container ports |
| `route` | `/`, `/api`, `mail.*` | Path prefix, or `sub.*` host route |
| `volume` | `<rel_path>` | CoW volume under the world subvolume |
| `size` | `"<hint>"` | Scheduling hint, default `"*"` |
| `dockerfile` | `<rel_path>` | Overrides `<dir>/Dockerfile` |
| `stage` | `<name>` | Builds an earlier `FROM ... AS <name>` stage |
| `use env` | `<group>` | Imports a top-level env group |
| `env { ... }`, `env.KEY = <expr>` | — | Inline environment, merged last |
| `build_env { ... }` | — | Build-time keys; a collision with `env` fails |
| `setup`, `pre-deploy`, `post-deploy`, `failed-deploy` | `<argv...>` or `{ ... }` | Lifecycle hooks |
| `on` | `<expr>` | Conditional directives |

### Service handle properties

| Property | Type | Meaning |
|---|---|---|
| `private_domain` | string | Container-to-container host name |
| `private_url` | string | URL reachable from another container |
| `private_port` | integer | The service's container port |
| `public_route_path` | string | The declared public route |
| `public_url` | string | Public URL on the world origin |

## Resources

```text
postgres pg {
    version 18
    database app_db
    database analytics { pool = false }
}
redis cache
s3 media
domain docs
```

| Declaration | Properties |
|---|---|
| `<database>` | `host`, `port`, `user`, `db_name`, `url`, `jdbc` |
| `<redis>` | `host`, `port`, `user`, `url` |
| `<s3>` | `port`, `endpoint`, `bucket`, `region`, `url` |
| `<domain>` | `name`, `url` |
| `app` | `domain`, `url`, `id` |
| `deployment` | `id`, `date_create`, `date_event`, `cause`, `commit_sha`, `is_production`, `is_leader`, `env.name`, `env.branch.{name,repo}`, `env_group.<KEY>`, `product.{name,slug}` |

`version` defaults to `"18"` for PostgreSQL and `"7"` for Redis. Referencing a cluster (`pg.url`) instead of one of its databases is a static error that names the right child.

!!! warning "Passwords are not properties"
    Read credentials through expressions: `secret(<KEY>)`. There is no `.password` or `.ca_cert` accessor.

## Environment expressions

| Form | Example |
|---|---|
| String, int, bool | `"info"`, `5432`, `true` |
| Resource property | `app_db.url` |
| Local override | `var(LOG_LEVEL)` |
| Vault or provider secret | `secret(API_TOKEN)` |
| Fallback | `secret(API_TOKEN) ?? "dev"` |
| Generator | `secret(API_TOKEN) ?? random_base64(32)` |
| Concatenation | `"http://" + app.domain` |

Generators are `random_hex(<bytes>)`, `random_base64(<bytes>)`, `random_string(<len>)`, and `uuid4()`. They are valid only on the right of `secret(KEY) ??`, and their value is written once.

## Command interpolation

`run`, hooks, and tasks expand three tokens directly into their argv slot, with no word splitting:

| Token | Value |
|---|---|
| `$port` | The service's declared port |
| `$env.KEY` | A resolved environment value |
| `$<entity>.<prop>` | Any resource property |

`'...'` is literal, `"..."` interpolates, and a leading `FOO=bar` sets a per-command environment override. A secret-tainted argument prints as `[REDACTED]`.

## Features

```text
features {
    default = [dev, docs]
    dev     = [mail]
    mail
    docs
}
```

`default` is enabled unless `--no-default-features` is passed. Enabling a feature enables everything it lists, transitively; an unknown name or a cycle is a static error.

`on` expressions support `feat`, `!feat`, `a & b`, `a | b`, and `(...)`, with `!` binding tighter than `&`, and `&` tighter than `|`.

## Diagnostics

`parse_forkfile` collects every error in the file — active and inactive `on` branches alike — and renders each with a span:

```text
error: unknown feature `mial`
  --> Forkfile:9:12
   |
 9 | on mial { … }
   |    ^^^^ declare it in `features { ... }`
```

| Check | Example failure |
|---|---|
| Duplicate ids | Two `service api` blocks, or a service and an env group with one name |
| Unknown identifiers | `app_db.url` before `database app_db` |
| Invalid properties | `pg.url` → *did you mean database* `app_db.url`? |
| Unknown tokens | `$prot` or `$env.MISSING` in a `run` line |
| Invalid directives | `dockerfile` on a `from <image>` service |
| Bounds | A `port` outside `1..=65535`, or a malformed route |

## Programmatic use

`crates/slipfork-forkfile` exposes the parser, checker, and feature evaluator as a library, and the evaluated manifest round-trips to JSON for `slipforkd` and OSPP subscriptions.

```rust
pub fn parse_forkfile(source: &str) -> Result<ForkFile, Vec<Diagnostic>>;
```

`ForkFile::evaluate(&FeatureSet)` returns the world `slipforkd` boots. `slipfork lsp` exposes the same diagnostics to editors over stdio.
