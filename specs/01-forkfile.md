# 01 — The `Forkfile` Specification

Slipfork runs your cloud environment locally in a box; CI is the clutch. One `Forkfile` at your repository root declares backing resources, runtime `env` and secrets, and the `image`s that enclose the `service`s running inside them.

```forkfile
# forkfile: 0.1

features {
  dev = [mail]
  mail
}

domain main
postgres pg { database app_db }
redis cache
s3 uploads
s3 assets {
  policy public-read
  cdn edge
}

env common {
  DATABASE_URL   = app_db.url
  REDIS_URL      = cache.url
  UPLOADS_URL    = uploads.url
  ASSETS_CDN_URL = edge.url
  PUBLIC_URL     = self.url
  LOGIN_URL      = "${self.url}/login"
  COOKIE_DOMAIN  = main.name
  SECRET_KEY     = secret(SECRET_KEY, random_string(50))
}

image api . {
  on dev: stage dev
  use env common

  service web {
    port 8000
    route /api
    on dev: run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port
  }
}
```

## 1. Version header and identifiers

Line 1 is `# forkfile: 0.1` while Slipfork is pre-`1.0`; any other value fails before a container starts, with a `Forkfile:<line>:<col>` caret span. Hyphenated ids (`app-db`, `edge`) are valid everywhere, including property access, and every declaration shares one flat, globally unique namespace. `self` is reserved and can never be declared: it always means the app's own `self.url` (scheme + host), `self.domain` (bare hostname), and `self.scheme`.

```mermaid
flowchart LR
  Decl["Hostname id\ndomain | cdn | s3 | route domain <id>"] --> Check{"RFC 1123 label?\n[a-z0-9-], alnum edges\n1..=63 (s3: 3..=63, non-IPv4)"}
  Check -->|"yes"| Ok["Valid .localhost / S3 authority"]
  Check -->|"no"| Err["Static Forkfile:line:col error\n+ next: suggested slug"]
```

`domain`, `cdn`, `s3` and `route ... domain <id>` labels are lowercase `[a-z0-9-]` with alphanumeric edges; buckets are `3..=63` and never IPv4-shaped.

## 2. `image <id>` and build-time `arg`

Each `image` declares a rootfs and encloses the `service`s and hooks that run inside it, built and cached natively on Btrfs + `crun` with zero Docker dependency.

| Form | Meaning |
| :--- | :--- |
| `image api . { ... }` | Build `<dir>/Dockerfile` natively |
| `image api ./backend { ... }` | Build in `./backend`; that directory is the default `workdir` |
| `image mailpit from axllent/mailpit:latest { ... }` | Pull OCI layers over HTTPS |
| `arg KEY = <expr>` | Build-time value; `secret(...)` is rejected statically |

`workdir`, `dockerfile` and `stage` override inside the block. Build-time `arg`s are never runtime `env`, and a secret-tainted `arg` in any `on` branch fails the file.

## 3. `features` and `on <expr>`

`dev` is built in and active for every local command: it is the switch that bind-mounts the checkout at `/project`. `-F -dev` (or `--no-default-features`) drops it and runs each image's own `ENTRYPOINT`/`CMD` with no `/project` bind.

With `dev`, a Dockerfile-built image overlays its live build context at the target stage's effective `WORKDIR` and at any distinct whole-context `COPY`/`ADD` destination, so `slipfork exec` and `slipfork shell` see host edits immediately; an explicit `WORKDIR /project…` keeps that bind. A stage that never sets `WORKDIR` still runs from `/`, which cannot be overlaid, so a `dev` boot refuses before any pod starts (one line + `next:`); `-F -dev` keeps the image copy and its `WORKDIR`.

`features` declares extra switches and groups. The default base is `{dev} ∪ features.default`, and a group implies its members transitively, so `dev = [mail]` turns `mail` on whenever `dev` is on. `-F/--features` edits that set per command (`-F mail`, `-F +mail`, `-F -dev`, `-F dev,mail`), and `slipfork features` persists the same edits to `.slipfork/features.json`.

`on <expr>` accepts `dev`, `!dev`, `dev & mail` and `staging | preview`, switching Dockerfile stages, conditional `arg`s, hot-reloading `run:` commands and dev-only services. Slipfork type-checks every branch before booting anything.

`else` closes a guard instead of mirroring it: `on dev { … } else { … }` for blocks, `on dev: … else: …` for one-line items, and `else on <expr>` to chain a second condition. The plain `else` runs only when neither the `on` condition nor any earlier `else on` matched. Write one `on dev { … } else { … }` rather than mirroring the condition with its negation.

An inline `on <expr>: <item>` may be followed by `else` on the next line, including `else { … }`, `else: <item>`, and `else on <expr> …`. `slipfork fmt` canonicalizes any inline `on` that carries an `else` to the block form `on <expr> { <item> } else …`, so a reparse can never read `else` as a sibling declaration.

## 4. Resources, S3 hosts and `cdn`

| Declaration | Local behaviour | Typed properties |
| :--- | :--- | :--- |
| `postgres <id> { database <db> }` | PostgreSQL `18` (`version` optional) | `<db>.url` (secret), `.host`, `.port`, `.name`, `.user`, `.password` (secret) |
| `redis <id>` | Redis `7` (`version` optional) | `<id>.url`, `.host`, `.port` |
| `s3 <bucket>` | Private: anonymous `GET`/`HEAD` answer `403` | `<bucket>.url` (secret), `.endpoint`, `.bucket`, `.access_key`, `.secret_key` (secret) |
| `s3 <bucket> { policy public-read }` | Anonymous `GET`/`HEAD` answer `200`; writes need SigV4 | Bucket properties on `<bucket>` |
| `s3 <bucket> { policy public-read; cdn <id> }` | Bucket host plus a cloud CDN binding | bucket properties plus the CDN's `.domain` and `.url` |
| `domain <id>` | Primary `<world>.localhost` when declared first, else extra `<id>.<world>.localhost` | `<id>.name` (bare hostname; no scheme) |

Every `s3 <bucket>` always serves at `http://<bucket>.<world>.localhost`; a fork serves it at `http://<bucket>.<fork>--<world>.localhost`. No extra hostname is invented for an S3 bucket.

`cdn <id>` inside an `s3` block is a **cloud** binding only. Locally, `cdn <id>`'s `.domain` and `.url` resolve to the bucket's own `<bucket>.<world>.localhost` and `http://<bucket>.<world>.localhost`; adding a CDN never adds a separate local `<id>.<world>.localhost` hostname.

The first declared `domain` is the primary `<world>.localhost`; every later one adds `<id>.<world>.localhost`. A `domain` is a bare hostname: `<id>.name` is its only property. A URL always carries a scheme, so use `self.url` for the app's primary URL, or `"${self.scheme}://${dom_b.name}"` for another declared domain. Any URL property on a `domain` is rejected statically.

!!! warning "`cdn <id>` requires explicit `policy public-read`"

    `s3 <bucket>` defaults to private, and Slipfork never silently makes a private bucket public. Declaring `cdn <id>` without `policy public-read` fails statically:

```text
error: 'cdn edge' inside 's3 assets' requires 'policy public-read'
  --> Forkfile:18:13
   |
18 | s3 assets { cdn edge }
   |             ^^^^^^^^
next: add 'policy public-read' inside 's3 assets' to enable 'cdn edge'
```

## 5. Runtime `env` and secrets

`env` (`env <group>`, `use env <group>`, inline `env { ... }`) is strictly runtime: it reaches services and hooks on an in-memory `tmpfs`, never a `Dockerfile` build.

`var(KEY)` reads non-secret config; `var(KEY, <generator>)` generates a non-secret value the first time the key is unset. `secret(KEY)` resolves the live cloud secret over `OSPP v1` when paired and persists a local fallback otherwise; `secret(KEY, <generator>)` names the generator used while offline. The generators are `random_string(n)` (alphanumeric), `random_hex(n)`, `random_base64(n)` and `random_uuid()`.

A generator is **only** valid as the second argument of `secret`/`var`: token, length and encoding are a secret-material concern, not a config default. `??` takes a deterministic expression, so write `secret(SECRET_KEY, random_string(50))` — never `secret(SECRET_KEY) ?? random_string(50)`, which fails statically. Use `??` for deterministic config such as `var(THIRD_PARTY_API_URL) ?? "https://thirdparty.com"`.

A double-quoted string interpolates with `"${<expr>}"`, so URLs compose without concatenation: `LOGIN_URL = "${self.url}/login"` or `"${self.scheme}://${dom_b.name}/api"`. Secret-tainted values print as `[REDACTED]` and zeroize on drop.

## 6. Ports, routes and domains

Each HTTP `service` declares one public `port <u16>`, available as `$port` and injected as `PORT`, plus optional `internal_ports [8081, ...]`. Routes preserve the matched prefix by default.

| Route | Path match | Authority |
| :--- | :--- | :--- |
| `route /api` | `/api/users` -> `/api/users` | primary `<world>.localhost` |
| `route /api strip-prefix` | `/api/users` -> `/users` | same |
| `route /api rewrite /v1` | `/api/users` -> `/v1/users` | same |
| `route exact /healthz` | `/healthz` only | same |
| `route / domain admin` | `/` | `admin.<world>.localhost` |
| `route / domain mail` | `/` | `mail.<world>.localhost` |

The first declared `domain` **is** the primary world domain: `main.name` above evaluates to `<world>.localhost`, so an unfiltered route and `route / domain main` both answer on `http://<world>.localhost`. Later declared domains only add subdomains.

`strip-prefix` and `rewrite <path>` are mutually exclusive, and a route naming an undeclared `domain` fails statically. A service that wants its own host declares `domain <id>` inline or inside `on <expr> { … }` and writes `route / domain <id>`:

```forkfile
on mail {
  domain mail
  image mailpit from axllent/mailpit:latest {
    service mailpit {
      port 8025
      route / domain mail
    }
  }
}
```

## 7. Services, hooks and `@<tool>` prequels

- **Hooks and `run`:** `service <id>` supports `before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, and `run:` / `run { ... }` (optional with an OCI `ENTRYPOINT`/`CMD`), expanding `$port` and `$env.KEY` without shell word-splitting.
- **`on <expr>` inside command blocks:**

  ```forkfile
  service front {
    before-start when changed(package.json, pnpm-lock.yaml) {
      on dev {
        pnpm install
      } else: pnpm install --frozen-lockfile
    }
    run {
      on dev {
        @pnpm exec vite dev --host 0.0.0.0 --port $port
      } else {
        pnpm install
        node build
      }
    }
  }
  ```

  `on <expr>: <cmd>` and `on <expr> { ... }` guard individual lines inside every hook block, nesting to any depth; `else: <cmd>`, `else { ... }` and `else on <expr>` close the guard, on the same or the next line. Each guarded condition is type-checked statically and inactive branches never run, while `on` stays a literal command word unless a complete condition is followed by `:` or `{`.
- **`before-start when changed(...)`:** hashes inputs against `.slipfork/` stamps, so an unchanged step skips in under 1 ms and the stamps travel into every fork.
- **`@<tool>` auto-prequels:** `@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go` and `@cargo` synthesize one deduplicated `(image, lockfile_dir)` install node.
- **`@<tool>` means "sync at boot":** the prequel marker belongs inside `on dev`, because it always synthesizes that deduplicated `uv sync` / `pnpm install` node — including dev dependencies. A hook treats `@<tool>` only as that boot-time prequel marker; the hook runner never rewrites it to a direct binary.
- **Slipfork never rewrites `uv`:** `uv run` executes verbatim, so it syncs per `uv`'s own rules. A non-`dev` world should write `uv run --no-sync …` explicitly in `run:` and in `before-start`/`pre-deploy`/`post-deploy`/`failed-deploy`; `slipfork exec` is passed through untouched too.

## 8. Validation and tooling

Slipfork type-checks every `on` combination before booting anything, with `Forkfile:line:col` carets and one `next:` remedy. It ships comment-preserving `slipfork fmt` (`--check`), a standalone cross-platform `slipfork-lsp` stdio server, and a Zed extension.
