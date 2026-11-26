# Tutorial: from a Python API to a forked world

Four steps add a Python API, a PostgreSQL database, a SvelteKit front, and a
fork of the running stack. All of it lives on one `.localhost` domain.

You need Linux with Btrfs and `crun`; on ext4 or XFS, [mount a workspace
image](install.md) first.

## 1. A Python API

```console
$ uv init hello
$ cd hello
$ uv add fastapi uvicorn
```

Save both files in `hello/`.

```dockerfile
# Dockerfile
FROM python:3.13-slim AS dev

# the build-time port, supplied by the Forkfile
ARG port

# uv installs the app dependencies on boot
RUN pip install --no-cache-dir uv

# `dev` mounts your live checkout here
WORKDIR /project
```

```forkfile
# forkfile: 0.1

image hello . {
  on dev: stage dev
  arg port = web.port

  service web {
    port 8000
    route /

    on dev {
      run: @uv run uvicorn app:api --reload --host 0.0.0.0 --port $port
    }
  }
}
```

**What this says**

- `image hello .` names an image and builds the `Dockerfile` in this directory.
- `FROM … AS dev` names the stage; `stage dev` picks it for `dev` boots.
- `ARG port` receives the value passed by `arg` at build time.
- `RUN` installs uv into the image while it is built.
- `WORKDIR /project` is where your live checkout is mounted.
- `arg port = web.port` passes the service port into the build as `port`.
- `service web` declares the pod that runs your app; `web` is its id.
- `port 8000` is the public port, injected as `PORT` and read as `$port`.
- `route /` gives every path on the world domain to this service.
- `on dev { … }` holds lines that only apply while `dev` is active.
- `run:` is the boot command for this service, replacing its own `CMD`.
- `$port` becomes the declared port inside that command.

```python
# app.py
from fastapi import FastAPI

api = FastAPI()

@api.get("/")
def index():
    return {"hello": "world"}
```

The stage's `WORKDIR` is where your checkout appears live, so edits reach the
pod without a rebuild; a stage without one refuses a `dev` boot. `arg` fills
`ARG port` before the image is sealed, and `$port` fills the boot command.

```console
$ slipfork up
✓ Ready -> http://hello.localhost
```

## 2. Add PostgreSQL

```forkfile
postgres pg { database app_db }

env common {
  DATABASE_URL = app_db.url
}

image hello . {
  use env common
  on dev: stage dev
  arg port = web.port

  service web {
    port 8000
    route /

    on dev {
      run: @uv run uvicorn app:api --reload --host 0.0.0.0 --port $port
    }
  }
}
```

**What this says**

- `postgres pg` creates a PostgreSQL 18 pod named `pg`.
- `{ database app_db }` creates a database called `app_db` inside that pod.
- `app_db.url` is the connection URL, known only once the pod is ready.
- `env common` groups runtime variables under one reusable name.
- `DATABASE_URL` is the variable your app will read.
- `use env common` attaches that group to the image for the service.

The Dockerfile does not change: `env` values are runtime only, so they arrive
after the pod starts, never during a build.

```python
# app.py (append)
import os

import psycopg

@api.get("/api/db")
def db():
    with psycopg.connect(os.environ["DATABASE_URL"]) as conn:
        row = conn.execute("select version()").fetchone()

    return {"version": row[0]}
```

```console
$ uv add "psycopg[binary]"

$ slipfork up
✓ Ready -> http://hello.localhost

$ curl -s http://hello.localhost/api/db
{"version":"PostgreSQL 18…"}
```

## 3. Put a SvelteKit front on the same domain

Scaffold the app next to the API, choosing pnpm when asked:

```console
$ npx sv create front
```

Save its Dockerfile as `front/Dockerfile`:

```dockerfile
# front/Dockerfile
FROM node:22-slim AS dev

RUN corepack enable

# `dev` mounts `./front` here
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

**What this says**

- `image frontend ./front` builds and works from `./front`.
- `service front` is a second id, and service ids are unique across the file.
- `port 5173` is Vite's port, declared on the service, not under `on dev`.
- `route /` is the catch-all route for the domain.
- `@pnpm` runs a lockfile-watched install before the dev server starts.

Now move the API onto a prefix:

```forkfile
image hello . {
  use env common
  on dev: stage dev
  arg port = web.port

  service web {
    port 8000
    route /api

    on dev {
      run: @uv run uvicorn app:api --reload --host 0.0.0.0 --port $port
    }
  }
}
```

**What this says**

- `route /api` claims that prefix and forwards it unchanged to the API.

The ingress picks the longest match, so `/` still reaches the front. Each image
mounts its own checkout at its `WORKDIR`, so both servers see your edits.

Both share one origin: no CORS, proxy or absolute API URL is needed.

```console
$ slipfork up
✓ Ready -> http://hello.localhost
```

## 4. Fork the world

```console
$ slipfork fork try-it
✓ try-it (5.2s) -> http://try-it--hello.localhost
```

The fork's checkout lands under `.forks/try-it`, and its data starts as a
snapshot of the parent's. Both worlds run at once, and writes in one stay out
of the other.

```console
$ slipfork status
```

The down verb stops a world but keeps its data. Delete the fork when done:

```console
$ slipfork drop try-it
```

`drop` stops the fork and deletes its snapshots; the parent keeps running.

## Where next

* [Install](install.md) — packages, the Btrfs workspace image and uninstall.
* [The Forkfile](forkfile.md) — resources, routes, hooks and secrets.
* [CLI](cli.md) — every verb, `--json` and the terminal contract.
