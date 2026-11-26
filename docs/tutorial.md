# Tutorial: from a Python API to a forked world

Four steps, each adding one file or one block. You finish with a Python API, a PostgreSQL database and a SvelteKit front on one `.localhost` domain — then fork the whole running stack.

You need Linux, Btrfs and `crun`; [Install](install.md) has the one-liner and the workspace image for ext4 or XFS hosts.

## 1. A Python API

Create the project with `uv init hello && cd hello && uv add fastapi uvicorn`, then save the `Dockerfile` and `Forkfile` below in `hello/`.

```dockerfile
# Dockerfile
FROM python:3.13-slim AS dev
RUN pip install --no-cache-dir uv
WORKDIR /project
```

```forkfile
# forkfile: 0.1

image api . {
  on dev: stage dev
  service web {
    port 8000
    route /
    on dev: run: @uv run uvicorn app:api --reload --host 0.0.0.0 --port $port
  }
}
```

`WORKDIR /project` is where `dev` bind-mounts your checkout, so the pod and your editor edit the same files. The `Forkfile` picks its `dev` stage and runs the hot-reloading command inside it.

```console
$ cat > app.py <<'PY'
from fastapi import FastAPI
api = FastAPI()
@api.get("/")
def index(): return {"hello": "world"}
PY
$ slipfork up
✓ Ready -> http://hello.localhost
```

## 2. Add PostgreSQL

`postgres` creates the database; `env` hands its DSN to the running service, never to the image build. The Dockerfile stays as it is.

```forkfile
postgres pg { database app_db }
env common {
  DATABASE_URL = app_db.url
}
```

```forkfile
image api . {
  use env common
  on dev: stage dev
  service web {
    port 8000
    route /
    on dev: run: @uv run uvicorn app:api --reload --host 0.0.0.0 --port $port
  }
}
```

Add the dependency and a route that uses it:

```python
import os, psycopg
@api.get("/api/db")
def db():
    with psycopg.connect(os.environ["DATABASE_URL"]) as conn:
        return {"version": conn.execute("select version()").fetchone()[0]}
```

```console
$ uv add "psycopg[binary]"
$ slipfork up
✓ Ready -> http://hello.localhost
```

## 3. Put a SvelteKit front on the same domain

`route /api` sends the API its prefix; `route /` sends everything else to the front. The longest prefix wins, so both answer on `http://hello.localhost`.

Scaffold the app, choosing pnpm, then change the API's `route /` to `route /api`.

```console
$ npx sv create front
```

```dockerfile
FROM node:22-slim AS dev
RUN corepack enable
WORKDIR /project/front
```

```forkfile
image frontend ./front {
  on dev: stage dev

  service front {
    port 5173
    route /

    on dev {
      run: @pnpm exec vite dev --host 0.0.0.0 --port $port
    }
  }
}
```

```console
$ slipfork up
✓ Ready -> http://hello.localhost
```

Fetch `/api/db` from the browser; the ingress routes `/api` to the API and `/` to Vite.

## 4. Fork the world

```console
$ slipfork fork try-it
✓ try-it (5.2s) -> http://try-it--hello.localhost
```

The fork's checkout lands in `.forks/try-it` under the project, and its data is a snapshot of the parent's. Both worlds run at once; rows and edits in one stay out of the other.

```console
$ curl -s http://try-it--hello.localhost/api/db | jq -r .version
$ slipfork status
$ slipfork drop try-it
```

`status` lists the parent and the fork, with the fork's checkout path. `drop` stops the fork and deletes its snapshots, while `slipfork down` stops the parent and keeps its data for the next `up`. `slipfork shell pg` opens `psql` on either database.

## Where next

* [Install](install.md) — packages, the Btrfs workspace image and uninstall.
* [The Forkfile](forkfile.md) — resources, routes, hooks and secrets.
* [CLI](cli.md) — every verb, `--json` and the terminal contract.
