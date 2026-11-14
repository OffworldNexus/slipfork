# Getting Started

Boot a full-stack project locally and run its test suite in parallel CoW worlds in **three steps** — with zero `.env` copy-pasting and zero `root:root` files.

## Prerequisites

- **Linux** (`x86_64` or `aarch64`) with either:
  - **[Incus](https://linuxcontainers.org/incus/) (`incusd`)** for full system-container & QEMU VM copy-on-write (disk + stateful RAM snapshots), **or**
  - **Rootless CoW filesystem** (`XFS` with `reflink=1`, `Btrfs`, or `ZFS`) + unprivileged user/network namespaces (works out of the box inside nested CI and LXC containers).

Check your host in one command:

```console
$ slipfork doctor
engine: incus (/var/lib/incus/unix.socket, pool=default [zfs], shift=true)
reflink: ok (/home/alice -> xfs FICLONE 16 MiB in 1.8 ms)
netns:   ok (CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWPID)
```

---

## 1. Switch `snow.yml` to `modes`

Replace top-level `envs:` with `modes:` (`all` + `cloud` + `local`). Use `!! random_string(50)` for any secret that can be generated automatically on first boot:

```yaml title="snow.yml"
images:
  - id: api
    context: api
  - id: front
    context: front

components:
  - id: api
    image: api
    envs: [django]
  - id: api-migrate
    image: api
    envs: [django]
  - id: front
    image: front
    envs: [front]

ingresses:
  - match: { prefix: "/back" }
    target: { component: api, port: 8000 }
  - match: { prefix: "/" }
    target: { component: front, port: 3000 }

hooks:
  - when: pre-deploy
    component: api-migrate
    run_command: uv run python manage.py migrate

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: psql, cluster: psql }]
  redis_clusters: [{ id: redis, version: "8" }]

modes:
  all:
    envs:
      django:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.psql.url }}'
        REDIS_URL: '${{ resources::redis_clusters.redis.url }}'
      front:
        API_URL: '${{ components::api.private_url }}'
  cloud:
    envs:
      django:
        SENTRY_DSN: '${{ vars::API_SENTRY_DSN }}'
  local:
    envs:
      django:
        DEBUG: 'True'
```

!!! note "Already have a legacy `snow.yml` with `envs:`?"
    Run `slipfork init` to convert top-level `envs:` into `modes.all` + `modes.cloud` + `modes.local` automatically.

---

## 2. Start your local world

```console
$ slipfork up
[secrets] generated secrets::SECRET_KEY -> local vault (~/.local/share/slipfork/vaults/acme.json)
[world]   psql:18 ready (5432), redis:8 ready (6379)
[hook]    pre-deploy (api-migrate): uv run python manage.py migrate (410 ms)
[ingress] http://acme.localtest.me -> /back:8000, /:3000 (uid=1000:1000)
```

Run any command inside the live world — your working tree is mounted at the **exact same path** under your own UID:

```console
$ slipfork exec -- uv run python manage.py makemigrations
Migrations for 'catalog':
  api/catalog/migrations/0002_product_sku.py   # owned by alice:alice, never root:root
```

---

## 3. Run the test suite in parallel CoW clones

```console
$ slipfork test
[dag] artifact front:  cached (sha256:9f2c1a4e, 0 ms)
[dag] snapshot migrate: cached (snap:mig-48ab, 0 ms)
[dag] snapshot seed:    cached (snap:seed-c019, 0 ms)
[run] 48 tests across 12 CoW worlds ... 48 passed in 3.4s
```

Every test ran against its own isolated PostgreSQL, Redis, and HTTP stack on `:8000`/`:3000` — zero `serialized_rollback`, zero port collisions, and zero leftover state.
