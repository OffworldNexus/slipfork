# Demo: a real PostgreSQL-backed world

One small Python API, one `postgres:18` cluster, one database, and one `/` ingress. `slipfork up` turns it into a running Copy-on-Write world with a port-less origin.

```console
$ cd ~/demo
$ slipfork up
$ curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
200
$ slipfork ps
$ slipfork fork demo-a            # second world at http://demo-a.demo.localhost
```

!!! info "No port to remember"
    `slipfork daemon install` gives systemd ownership of `:80`, so the origin is `http://demo.localhost`. When a world runs on an ephemeral port instead, `slipfork ps` prints the port.

`tools/do_vm.py sync` seeds this directory once at `/home/slipfork/demo` as one Git commit. Later edits are yours; a later `sync` never overwrites a seeded demo.

## What is inside

| File | Purpose |
|---|---|
| `app/server.py` | The API component on internal port `8000` |
| `db/*.sql` | Schema and seed, applied idempotently on every boot |
| `Dockerfile` | The `api` build image (`FROM python:3.12-slim AS app`) |
| `snow.yml` | `images`, `components`, `ingresses`, `resources.pg_*`, `modes.all` / `modes.local` |
| `slipfork.toml` | `project = "demo"`; world and fork ids derive from it |

The component runs `app/server.py` **from this checkout**, bind-mounted at its same path inside the world. Edit the API or the SQL and run `slipfork restart`; there is no Docker rebuild.

`modes.all` carries only values every host resolves (`DATABASE_URL`, `PGUSER`). `modes.local` carries the local-tier `!!` secret (`DEMO_SECRET`) plus `DEBUG`. Auto-generated secrets belong to the mode tier that consumes them.

## Forks

```console
$ slipfork fork demo-a     # .worktrees/demo-a plus its own subvolume and origin
$ slipfork drop demo-a     # stop it and remove the worktree
```

Each fork is its own Btrfs subvolume, its own `postgres:18` cluster, and its own subdomain. The canonical world and every fork run at the same time.

## Database and logs

```console
$ slipfork db demo         # psql 18 inside the world's postgres container (needs a TTY)
$ slipfork logs api        # retained component output
$ slipfork ps              # every component and crun service in the world
```

`db` takes the declared `pg_databases.<id>`; with exactly one declaration the argument can be omitted. Each fork starts from the same tables and adds its own `boot` row scoped to the fork.

Cookbook: [`docs/getting-started.md`](../../docs/getting-started.md) and [`docs/snow-modes.md`](../../docs/snow-modes.md).
