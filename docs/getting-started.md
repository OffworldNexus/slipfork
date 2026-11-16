# Getting Started (`0.2.0`)

Go from a repository to a running, isolated development world in four commands after `cargo install` — zero `root:root` files.

```console
$ cargo install --path crates/slipfork
$ slipfork doctor
$ slipfork snow resolve --file snow.yml --mode local
$ slipfork up
$ slipfork list
```

Every world boots from a read-only rootfs built from your own `Dockerfile`, so `snow.yml` must declare at least one build image:

```yaml title="snow.yml"
images:
  - id: api
    context: api
    # Dockerfile: FROM python:3.13-slim AS app

modes:
  local: {} # the only mode `slipforkd` boots
```
Both parts are required: a build image with a **named final Dockerfile stage**, and a `modes.local` entry. If `slipfork up` reports no buildable image, add the `images:` snippet; if it reports `mode 'local' is not defined`, add `modes: { local: {} }`. A project that cannot be built into an image cannot be forked; projects using an upstream image write a one-line `FROM <image> AS app` Dockerfile.

Prerequisites: **Linux** (`x86_64`/`aarch64`), **Rust 1.94+** (`edition = "2024"`), Docker with `buildx` (the rootfs is exported with `docker buildx build --output type=local`), a non-bare `git` repository (a colocated Jujutsu checkout resolves through its Git store), `unshare`/`ip`, and a CoW filesystem (`XFS` with `reflink=1`, `Btrfs`, or `ZFS`) under `~/.cache/slipfork` — never RAM-backed `/tmp` `tmpfs`.

## 1. Probe the host

```console
$ slipfork doctor
state_root: /home/acme/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 414 us)
netns:      ok (uid=1000:1000)
```

`reflink` and `netns` must both report `ok`; `doctor` exits non-zero otherwise. `slipfork --json doctor` emits the same probe as one object.

## 2. Resolve `snow.yml`

Top-level wiring plus `modes.all` for shared values and `modes.local` for your machine:

```yaml title="snow.yml"
images:
  - id: api
    context: api

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:${{ components::api.private_port }}
    envs: [app]
    internal_ports: [8000]

hooks:
  - component: api
    when: pre-deploy
    run_command: uv run alembic upgrade head

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: appdb, cluster: psql }]
  redis_clusters: [{ id: cache, version: "8" }]

modes:
  all:
    envs:
      app:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.appdb.url }}'
  local:
    envs:
      app:
        DEBUG: 'True'
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
```

```console
$ slipfork snow resolve --file snow.yml --mode local
DATABASE_URL=postgres://127.0.0.1:5432/appdb
DEBUG=True
LOG_LEVEL=INFO
SECRET_KEY=[REDACTED]
```

`!!` fills a missing secret once and persists it in a `0600` vault; re-running after a manifest edit keeps the value. A top-level `envs:` or `project:` block is refused by key name only. Details: [`snow.yml` modes & secrets](snow-modes.md).

## 3. Boot the canonical world

```console
$ slipfork up
[resources] appdb: prefer_remote -> local (not_offered)
[ready]     api (internal :8000 in world netns)
[origin]    http://acme.localhost:41921

$ slipfork list
WORLD            STATE     ORIGIN                                   WORKTREE
acme             running   http://acme.localhost:41921              /home/acme/acme
```

The first `up` builds and caches the rootfs under `<state_dir>/cache/rootfs/<sha256>/app`, then starts one `crun` OCI container per declared resource (`postgres:<v>`, `redis:<v>`, SeaweedFS), all sharing the world's `127.0.0.1`, and your components as `$UID:$GID` processes on the read-only rootfs. Repeating `up` is an idempotent confirmation that prints the same single origin URL.

- **Zero setup:** when `<runtime_dir>/daemon.sock` does not answer, `slipfork` starts `slipforkd --listen 127.0.0.1:0 --state-dir <state> --runtime-dir <runtime>` in the background (`<runtime_dir>/daemon.log`, `0600`); `HOME` must be set before any mutation.
- **Host toolchain caches (optional):** `slipfork up --host-caches` mounts `~/.cargo`, `~/.rustup`, `~/.cache/uv`, `~/.local/share/uv`, and the pnpm store read-only — but only after the ELF/libc ABI probe passes, and never as a `slipfork.toml` key.

## 4. Fork a second world

```console
$ slipfork fork feat-billing
[origin]    http://feat-billing.acme.localhost:41921

$ slipfork exec --world feat-billing -- id
$ slipfork down --world feat-billing   # stop services, keep .worktrees/feat-billing
$ slipfork drop feat-billing           # destroy the world, worktree, branch, and snapshot ref
$ slipfork prune --dry-run             # preview orphaned worlds (no .worktrees/<fork> left)
```

A fork is a Git worktree (`.worktrees/<fork>` + `refs/slipfork/<fork>/wip`) plus its own CoW world, created in one daemon call. If the checkout carries `.jj`, the fork is a Jujutsu workspace instead: `slipfork fork` runs `jj workspace add`, projects `@-`/`@` into the colocated Git store, and fails closed with `ToolMissing` when `jj` is not on `PATH` — never a silent Git fallback. Either way the project slug resolves through the Git store. `slipfork exec -- <cmd>` runs non-interactively in the world with the daemon-resolved environment; `slipfork shell` enters it interactively (TTY required).

## 5. Work inside the running world

All of these resolve the world from `cwd` or `--world`, and need the world running:

```console
$ slipfork db appdb                          # psql inside the postgres:<v> container (TTY)
$ slipfork redis cache                       # redis-cli inside the redis:<v> container (TTY)
$ slipfork logs api                          # retained component/hook output; --follow streams
$ slipfork hook run api:pre-deploy --no-tty  # re-run one hook; omit --no-tty for a native TTY
$ slipfork forward pg_databases.appdb        # 127.0.0.1:<ephemeral> bridged into the world
```

`db`/`redis` fail closed with `ServiceNotRunning` when that container is stopped; non-interactive agents can run `slipfork forward pg_databases.appdb` and connect with host `psql` instead. `forward` prints its bind and holds until Ctrl-C:

```console
$ slipfork forward pg_databases.appdb
[forward] pg_databases.appdb       postgres://127.0.0.1:33803/appdb (127.0.0.1:33803)
```

## 6. Optional: a systemd daemon

```console
$ slipfork daemon unit --only socket | sudo tee /etc/systemd/system/slipforkd.socket
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket
```

The units are rendered to stdout with zero filesystem writes, resolve your account as `User=`, and bind `127.0.0.1:80`, `[::1]:80`, and `127.0.0.1:7080`. This optional setup is the one place that needs `sudo` (to install the units and bind port 80); the default auto-spawn path needs none. Without them, auto-spawn is enough for laptops, CI, and agents. Sharing (`slipfork share`) still fails closed until the `weaved` tunnel adapter lands; use each world's `.localhost` origin.

## Where to go next

- [`snow.yml` modes & secrets](snow-modes.md) — mode merge rules, `disabled: true`, and the single-origin coordinate model.
- [Orthogonal DAG planner](testing-dag.md) — configure `slipfork.toml` and read Merkle rebuild waves.
- [Manifest reference](reference/manifest.md) — both schemas, `[world]` selection, and validation rules.
- [OpenCode V2 plugin](agents-and-opencode.md) — automatic worktree worlds for coding agents.
