---
description: "Slipfork is a cloud in a box: setup any project and its infrastructure in 5s on your laptop and get your coding agent fork it at will in milliseconds."
hide:
  - toc
---

![A barn swallow in flat poster colours](brand/mascot.svg){ align=right width=220 }

# Your cloud environment, locally in a box

Slipfork is like Docker Compose with opinions, without Docker and on crack. It understands the application's managed dependencies (database, storage, etc) through an environment-agnostic `Forkfile`, and is able to create a *cloud in box* completely automatically. You go from **git checkout** to a **running development environment** in seconds.

It's not based on Docker because it tries to circumvent the typical hurdles that you meet when doing development:

- Automatic UID mapping, so that if you execute something inside the container it won't create files as root in your home folder
- Every project gets their own `.localhost` domain name, you never need to worry about port collision again
- Supporting infrastructure such as PostgreSQL, Redis or S3 are bootstrapped as needed: all you need is ask

!!! warning "Targeted platform support"

    The project is leveraging advanced features of the Linux kernel and the Btrfs file system. If you're on Linux but don't have Btrfs, [mount a Btrfs workspace image](install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder). If not, you will need to run it from a VM. The target user runs this on a cloud machine, alongside a coding agent.

!!! warning "Pre-1.0"

    **Pre-1.0.** The Forkfile and CLI can change between releases; report what breaks in the
    [issue tracker](https://github.com/OffworldNexus/slipfork/issues).

## Getting started

First, let's install slipfork on your local machine:

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
```

Next, you need a project (with Btrfs — [mount a workspace image](install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder) on ext4 or XFS). We're going to host a simple Python/WSGI application. The Forkfile looks like this:

```forkfile
# forkfile: 0.1

postgres pg { database app_db }

env common {
    DATABASE_URL = app_db.url
}

image api . {
    arg port = web.port
    use env common

    on dev {
        stage dev
    }

    service web {
        port 4242
        route /

        on dev {
            run: @uv run granian --reload --interface wsgi app:app --host 0.0.0.0 --port $port
        }
    }
}
```

Now create the project and its files:

```console
$ uv init get-started
$ cd get-started
$ uv add granian
```

```python
# app.py
def app(environ, start_response):
    start_response("200 OK", [("Content-Type", "text/plain; charset=utf-8")])
    return [b"hello from get-started\n"]
```

```dockerfile
# Dockerfile
FROM python:3.13-slim AS dev
RUN pip install --no-cache-dir uv
WORKDIR /project
```

`WORKDIR /project` is where the `dev` stage bind-mounts your checkout. Save the `Dockerfile`, `Forkfile` and `app.py` in `get-started/`, then:

```console
$ slipfork up
✓ Ready -> http://get-started.localhost
```

The project directory name is the world name, so this world answers on `http://get-started.localhost`.

The [tutorial](tutorial.md) continues from here: adding a database, a SvelteKit front, and forking the running world.
