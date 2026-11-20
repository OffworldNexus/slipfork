# 01 — The `Forkfile` Specification

A `Forkfile` lives at the root of your repository and declares the cloud-and-local topology of your application: backing resources, runtime `env` & secrets, and container `image`s (with build-time `arg`s) enclosing the `service`s that run inside them.

```forkfile
# forkfile: 0.1

features {
  default = [dev]
  dev = [mail]
  mail
}

domain main
postgres pg { database app_db }
redis cache
s3 uploads
s3 assets { cdn static_cdn }

env common {
  DATABASE_URL         = app_db.url
  REDIS_URL            = cache.url
  UPLOADS_URL          = uploads.url
  AWS_S3_CUSTOM_DOMAIN = static_cdn.domain
  PUBLIC_URL           = main.url
  SECRET_KEY           = secret(SECRET_KEY) ?? random_hex(32)
  on mail: SMTP_ADDR   = "127.0.0.1:1025"
}

image backend . {
  on dev: stage dev
  use env common

  service api {
    port 8000
    route /api
    pre-deploy: @uv run alembic upgrade head
    run: uv run gunicorn app.main:app --bind 0.0.0.0:$port
    on dev: run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port
  }

  service worker {
    run: uv run celery -A app.worker worker
    on dev: run: @uv run celery -A app.worker worker
  }
}

image frontend ./front {
  arg VITE_CDN_URL = static_cdn.url
  on dev: stage dev

  service front {
    port 4173
    route /
    run: pnpm run preview -- --host 0.0.0.0 --port $port
    on dev {
      port 5173
      run: @pnpm run dev -- --host 0.0.0.0 --port $port
    }
  }
}

on mail {
  image mailpit from axllent/mailpit:latest {
    service mailpit {
      port 8025
      route / domain mail.*
    }
  }
}
```

---

## 1. Pre-`1.0` Format Header (`# forkfile: 0.1`)

While `< 1.0`, every `Forkfile` begins on line 1 with `# forkfile: 0.1`. Slipfork and CI check this magic comment upfront and fail fast with a clear version-mismatch diagnostic as the format evolves (`0.1` -> `0.2` ...); once `1.0` stabilizes, the header requirement drops cleanly without changing comment syntax.

## 2. `image <id>` & Build-Time `arg` (Enclosing Container Scope)

`image` defines both a container image and the scope for the `service`s and hooks running inside it — built and cached natively on Btrfs + `crun` with **zero Docker dependency**:

- **Built from a `Dockerfile`:** `image backend . { ... }` builds `<dir>/Dockerfile` natively on Btrfs (optional `dockerfile <path>`, target `stage <name>`, and build-time `arg`).
- **Pulled from an OCI registry:** `image mailpit from axllent/mailpit:latest { ... }` pulls layers over HTTPS via the OCI Distribution API into a cached read-only Btrfs subvolume.
- **Build-time `arg` (strictly non-secret):** `arg KEY = <expr>` and `arg { KEY = <expr> ... }` (also conditional via `on <expr>`, e.g. `arg VITE_CDN_URL = static_cdn.url`, `arg CDN_HOST = static_cdn.domain`, `arg COMMIT_SHA = deployment.commit_sha`) pass `Dockerfile` `ARG`s at build time. `arg` statically rejects `secret(...)` and any secret-tainted property so secrets can never bake into OCI image layers.
- **Automatic inheritance:** Every `service` and hook inside `image <id> <dir> { ... }` inherits the image rootfs, `<dir>` as default `workdir` (overridable via `workdir <subpath>`), any `use env` / `env { ... }` or `before-start` hooks on the `image`, and the live bind-mounted Btrfs project subvolume. Ad-hoc CLI commands (`pytest`, linters, scripts) run directly inside that environment via `slipfork exec [service|image] -- <cmd>` or `slipfork shell [service|image]`.

## 3. `features` & `on <expr>` — Local vs Cloud Parity

Cargo-style `features` let one `Forkfile` express both production and local development: `default` activates automatically on local commands (`slipfork up`), while CI evaluates production mode with `--no-default-features` (or explicit `--features`). `on <expr> { ... }` and `on <expr>: ...` accept boolean expressions (`dev`, `!dev`, `dev & mail`, `staging | preview`) to switch `Dockerfile` target stages, pass conditional `arg`s, swap `run:` to `@`-prefixed hot-reloaders, or add dev-only tools like `mailpit`.

## 4. Backing Resources & `s3 { cdn <id> }` Policy

Slipfork runs real containerized backing services locally matching cloud semantics (DigitalOcean Managed Databases, Spaces & App Platform; `slipfork migrate` converts legacy `snow.yml` -> `Forkfile`). Top-level resources and named child resources (`database <db>`, `cdn <id>`) share one flat, globally unique symbol namespace:

| Declaration | Local Behavior & Default Policy | Typed Properties |
| :--- | :--- | :--- |
| `postgres <id> { database <db> }` | PostgreSQL `18` (`version` optional) | `<db>.url`, `<db>.host`, `<db>.port`, `<db>.name`, `<db>.user`, `<db>.password` |
| `redis <id>` | Redis `7` (`version` optional) | `<id>.url`, `<id>.host`, `<id>.port` |
| `s3 <bucket>` | **Private by default** (anonymous `GET` -> `403`) | `<bucket>.url`, `<bucket>.endpoint`, `<bucket>.bucket`, `<bucket>.access_key`, `<bucket>.secret_key` |
| `s3 <bucket> { cdn <id> }` | **`public-read`** bucket + CDN (`http://<id>.<world>.localhost`) | Bucket properties on `<bucket>` + non-secret **`<id>.domain`**, **`<id>.url`** |
| `domain <id>` | Primary: `<world>.localhost`; extra: `<id>.<world>.localhost` | Non-secret **`<id>.domain`**, **`<id>.url`** |

- **`s3` private by default vs `cdn <id>`:** Plain `s3 uploads` denies anonymous HTTP `GET` (`403`) both in cloud Spaces and locally. Declaring `cdn <id>` (`s3 assets { cdn static_cdn }`) switches `assets` to **`public-read`**, provisions `http://static_cdn.<world>.localhost`, and exposes non-secret `static_cdn.domain` and `static_cdn.url` matching `domain <id>` 1:1.

## 5. Runtime `env` & Secrets (`env`, `var()`, `secret()`)

`env` (`env <group>`, `use env <group>`, inline `env { ... }`) is **strictly runtime** — injected into running `service`s and lifecycle hooks (`before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`), and never exposed to `Dockerfile` builds. `var(KEY) ?? "default"` reads non-secret config; `secret(KEY) ?? random_hex(32)` (`random_base64(n)`, `random_alnum(n)`, `random_uuid()`) resolves live cloud secrets over `OSPP v1` when paired or generates local offline secrets. Secret-tainted values are redacted in logs and CLI output.

## 6. Ports & Cloud-Identical Ingress Routing (`port`, `route`, `domain`)

Each HTTP `service` declares one public `port <u16>` (exposed as `$port` in `Forkfile` commands and auto-injected as `PORT` in runtime `env`, plus optional `internal_ports [8081, ...]` for private pod traffic). `route` preserves matched path prefixes by default (CI emits `preserve_path_prefix: true` on DigitalOcean App Platform automatically) and supports explicit `strip-prefix`, `rewrite`, `exact`, and `domain` rules matching cloud `ingress.rules` 1:1:

| `Forkfile` Route | Path Match & Prefix Handling | Authority (`Host`) Filter |
| :--- | :--- | :--- |
| `route /api` | Prefix `/api` — **preserves `/api` by default** (`/api/users` -> `/api/users`) | Any declared app `domain` |
| `route /api strip-prefix` | Prefix `/api` — strips `/api` (`/api/users` -> `/users`) | Any declared app `domain` |
| `route /api rewrite /v1` | Prefix `/api` — rewrites prefix (`/api/users` -> `/v1/users`) | Any declared app `domain` |
| `route exact /healthz` | Exact path `/healthz` only (no regex rules) | Any declared app `domain` |
| `route / domain admin` | Prefix `/` | Exact declared `domain admin` (`admin.<world>.localhost`) |
| `route / domain mail.*` | Prefix `/` | Subdomain prefix `mail.` (`mail.<world>.localhost`) |

`strip-prefix` and `rewrite <path>` are mutually exclusive (enforced statically), and `domain <id>` filters are verified against declared top-level `domain`s.

## 7. Services, `before-start when changed(...)` & `@<tool>` Auto-Prequels

- **Hooks & `run`:** `service <id>` supports `before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, and `run:` / `run { ... }` (optional when a `from <ref>` image defines an OCI `ENTRYPOINT`/`CMD`). Multi-line `{ ... }` blocks execute fail-fast line-by-line (in `service` `run { ... }`, the final line runs as the supervised process; `$port` and `$env.KEY` expand safely without word-splitting).
- **`before-start` & `when changed(...)`:** Inline prep/codegen steps (`before-start when changed(prisma/schema.prisma): @pnpm exec prisma generate` or `{ ... }`) hash matched files against `.slipfork/` stamps in the Btrfs project subvolume — running in the boot DAG only when inputs changed, skipping in `< 1ms` on unchanged `up`/`restart`, and inherited across `slipfork fork`.
- **`@<tool>` auto-prequel mode:** Prefixing `@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go`, or `@cargo` in `run:` or a hook invokes that tool **and** inserts a deduplicated `(image, lockfile_dir)` `before-start when changed(<lockfiles>)` install node (`uv sync`, `poetry install`, `pnpm install`, `npm install`, `yarn install`, `bun install`, `composer install`, `bundle install`, `go mod download`, `cargo fetch`), walking up to detect workspace root lockfiles.

## 8. Static Validation, Formatting & Editor Support

Slipfork validates `# forkfile: 0.1` and type-checks **every `on` feature branch at once** before starting any container (`Forkfile:line:col` caret spans) and ships first-party `slipfork fmt`, `slipfork lsp`, and a Zed extension.
