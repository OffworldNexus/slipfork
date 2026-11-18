# Slipfork `0.1.0` demo: a real PostgreSQL-backed world

One Python component (`app/server.py`) on internal port `8000`, one `postgres:18` cluster
(`psql`), one database (`demo`), one `/` ingress, and one `modes.local`. The origin is
**port-less**: systemd owns `:80` through `slipforkd.socket`, so a project is
`http://<project>.localhost` and there is no port to look up.

`tools/do_vm.py sync` seeds this directory once at `/home/slipfork/demo` as one Git commit.
Later edits are yours: a later `sync` never overwrites a seeded demo.

## Quickstart

```console
$ cd ~/demo
$ slipfork                 # daemon line, worlds, and the next command per world
$ slipfork up              # boots the world and prints its port-less origin
$ curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
200
$ slipfork list            # world ids, states, and one origin each
```

If nothing answers on the daemon's control socket yet, `slipfork up` is the verb that starts it
and boots the world.

## Forks

```console
$ slipfork fork demo-a     # `.worktrees/demo-a` plus its own Btrfs subvolume and origin
$ slipfork drop demo-a     # stops it and removes the worktree
```

Each fork is its own Btrfs subvolume, its own `postgres:18` cluster, and its own subdomain, so
the canonical world and its forks run at the same time.

## Database and logs

```console
$ slipfork db demo         # psql 18 inside the world's postgres container (needs a TTY)
$ slipfork logs api        # retained component output (the API logs its world and db readiness)
```

`db` takes the declared `pg_databases.<id>` (`demo`); with exactly one declaration the argument
can be omitted. The schema and seed live in `db/*.sql`; they are applied idempotently on every
boot, so a fork starts from the same tables and adds its own `boot` row scoped to the fork.

## Manifests

| File | Purpose |
|---|---|
| `snow.yml` | `images`, `components`, `ingresses`, `resources.pg_*`, `modes.all` / `modes.local` |
| `Dockerfile` | the `api` build image (`FROM python:3.12-slim AS app`) |
| `slipfork.toml` | `project = "demo"` — the world and fork ids derive from it |

`modes.all` carries only values every host resolves (`DATABASE_URL`, `PGUSER`); `modes.local`
carries the local-tier auto-generated secret (`DEMO_SECRET`) plus `DEBUG`. The two-tier rule:
auto-generated `!!` secrets belong to the mode tier that consumes them, never to `all`.

Cookbook: `docs/getting-started.md` and `docs/snow-modes.md` in the Slipfork repository.
