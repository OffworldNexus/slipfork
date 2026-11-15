# Getting Started (`0.1.0`)

Build `slipfork` from source, verify your Linux CoW filesystem and user/network namespaces, resolve a `snow.yml` v2 manifest, and run commands inside an isolated CoW world — with zero `sudo` and zero `root:root` files.

## 1. Build & Probe Host Capabilities

Prerequisites: **Linux** (`x86_64` / `aarch64`), **Rust 1.88+** (`edition = "2024"`), a standard non-bare `git` repository (or `jj` on `PATH`; see note below), `unshare` / `ip` (`util-linux` + `iproute2`), and a CoW-capable filesystem (`XFS` with `reflink=1`, `Btrfs`, or `ZFS`) under `~/.cache/slipfork` (never RAM-backed `/tmp` `tmpfs`).

!!! note "Project slug & Jujutsu (`jj`) workspaces in `0.1.0`"
    Slipfork derives `<project>` from the canonical `git` root (`git rev-parse --show-toplevel`, resolving linked `.worktrees/<fork>` back to the main repo; bare repos fail closed, and submodules / `--separate-git-dir` require `project = "<slug>"` in `slipfork.toml`). For Jujutsu, Slipfork runs `jj root` when `jj` is on `PATH` (failing closed with a `project = "<slug>"` hint when `jj` is absent); because `jj root` is workspace-scoped, `.worktrees/<fork>` workspaces under `jj` require `project = "<slug>"` in `slipfork.toml` until Milestone 2's `JjWorkspaceAdapter` lands ([Manifest Reference](reference/manifest.md)).

```console
$ cargo install --path crates/slipfork
$ slipfork doctor
state_root: /home/alice/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 447 us)
netns:      ok (uid=1000:1000)
```

Pass `--json` to any command for structured output (`slipfork --json doctor`).

---

## 2. Author & Resolve `snow.yml`

Inside a repository whose root directory is a lowercase DNS label (or sets `project = "acme"` in `slipfork.toml`), create a `snow.yml` file using `modes:` (`all` + `cloud` + `local`) and `!! random_string(50)` for write-once local secret generation:

```console
$ mkdir acme && cd acme && git init
```

```yaml title="snow.yml"
images:
  - id: api
    context: api

components:
  - id: api
    image: api
    run_command: gunicorn app.main:app --bind 0.0.0.0:${{ components::api.private_port }}
    envs: [app]

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: appdb, cluster: psql }]

modes:
  all:
    envs:
      app:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.appdb.url }}'
        APP_URL: '${{ resources::app.url }}'
  cloud:
    envs:
      app:
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
  local:
    components:
      api:
        # Validated in 0.1.0; process supervision ships in M2 (slipfork-edge)
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }} --reload
    envs:
      app:
        DEBUG: 'True'
```

Validate `snow.yml` and persist auto-generated `!!` secrets into your `0600` local vault (`~/.local/share/slipfork/vaults/<project>.json` — secrets stay `[REDACTED]` unless `--expose-secrets` is passed):

```console
$ slipfork snow resolve --file snow.yml --mode local
APP_URL=http://acme.localhost
DATABASE_URL=postgres://127.0.0.1:5432/appdb
DEBUG=True
SECRET_KEY=[REDACTED]
```

---

## 3. Snapshot, Fork, Execute & Drop a CoW World

Capture a CoW snapshot of any stateful directory (on XFS/Btrfs disk, not `/tmp`), clone an isolated world in milliseconds, run a command inside its private network namespace (`127.0.0.1` isolated, `snow.yml` envs injected, host `$UID:$GID` preserved), and drop it when finished:

```console
$ mkdir -p ./sample-state && echo "seeded" > ./sample-state/marker.txt
$ slipfork snap --source ./sample-state canonical
snapshot 'canonical' created in 1.12 ms

$ slipfork fork exp --from canonical --workspace .
forked world 'exp' -> /home/alice/.cache/slipfork/worlds/exp/state in 0.95 ms

$ slipfork exec --world exp --snow-file snow.yml -- sh -c 'echo "$APP_URL ($DATABASE_URL) uid=$(id -u)"'
http://acme.localhost (postgres://127.0.0.1:5432/appdb) uid=1000

$ slipfork drop exp
dropped world 'exp'
```
