# Getting Started (`0.1.0`)

Build `slipfork`, prove your host can do copy-on-write and unprivileged namespaces, resolve a `snow.yml` manifest, plan a build/fixture DAG, and snapshot state — with zero `sudo` and zero `root:root` files.

```console
$ cargo install --path crates/slipfork
$ slipfork doctor
$ slipfork snow resolve --file snow.yml --mode local
$ slipfork dag plan --file slipfork.toml --repo .
$ slipfork snap --source ~/pgdata canonical
```

!!! warning "World lifecycle needs `slipforkd` (`0.2.0`)"
    `slipfork fork`, `exec`, and `drop` exist, parse their flags (invalid flags exit `1`), and then exit `2`: worlds are owned by the `slipforkd` daemon, which lands in `0.2.0`. Everything on this page runs today.

---

## 1. Build & Probe the Host

Prerequisites: **Linux** (`x86_64` / `aarch64`), **Rust 1.94+** (`edition = "2024"`), a non-bare `git` repository (or `jj` on `PATH`), `unshare` / `ip` (`util-linux` + `iproute2`), and a CoW filesystem (`XFS` with `reflink=1`, `Btrfs`, or `ZFS`) under `~/.cache/slipfork` — never RAM-backed `/tmp` `tmpfs`.

```console
$ cargo install --path crates/slipfork
$ slipfork doctor
state_root: /home/acme/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 447 us)
netns:      ok (uid=1000:1000)
```

`reflink` and `netns` must both report `ok`; `doctor` exits non-zero otherwise. Add `--json` before any command for structured output (`slipfork --json doctor`).

!!! note "Project slug"
    `<project>` is your canonical `git` root's directory name (linked `.worktrees/<fork>` resolves back to the main repository). Git submodules and `--separate-git-dir` layouts fail closed until you set `project = "<slug>"` in `slipfork.toml`, and a bare repository has no working tree to develop in — clone it first. Under `jj`, Slipfork runs `jj root`, so `.worktrees/<fork>` workspaces need the same override, as does any `jj` workspace without `jj` on `PATH`.

## 2. Author & Resolve `snow.yml`

In a repository named after your project (here `acme`), write the manifest: top-level wiring plus `modes.all`, with per-mode differences in `modes.local`.

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
  local:
    components:
      api:
        run_command: uv run uvicorn app.main:app --host 127.0.0.1 --port ${{ components::api.private_port }} --reload
    envs:
      app:
        DEBUG: 'True'
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
```

Resolve it: Slipfork validates the manifest, persists the generated `SECRET_KEY` into the local `0600` vault, and prints the resolved `local` mode.

```console
$ slipfork snow resolve --file snow.yml --mode local
APP_URL=http://acme.localhost
DATABASE_URL=postgres://127.0.0.1:5432/appdb
DEBUG=True
LOG_LEVEL=INFO
SECRET_KEY=[REDACTED]
```

Change one character in `snow.yml` and re-run: `SECRET_KEY` keeps its value, because `!!` fills a missing secret exactly once. A top-level `envs:` or `project:` block is refused with the key name only.

## 3. Plan the Artifact & Fixture DAG

`slipfork.toml` describes stateless builds and stateful fixture lineages:

```toml title="slipfork.toml"
project = "acme"

[artifacts.front]
inputs = ["front/src/**", "front/package.json"]
build = "pnpm --dir front build"
mount = "front/dist"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "psql -f api/migrations/0001_schema.sql"
```

```console
$ slipfork dag plan --file slipfork.toml --repo .
artifact front            Rebuild (5fe1ef1ecec6)
wave 0 fixture migrate      Rebuild (cd305423c952)
```

`Rebuild` means the digest is not in `<STATE_DIR>` yet — and `dag plan` only reads the cache, so every node reports `Rebuild` until something materializes it (M3). Editing `front/src/**` changes only the `front` digest, and editing `api/migrations/**` changes only `migrate` and its descendants.

## 4. Snapshot State

```console
$ mkdir -p ./pgdata && echo seeded > ./pgdata/marker.txt
$ slipfork snap --source ./pgdata canonical
snapshot 'canonical' created in 0.36 ms
```

The snapshot lives at `<STATE_DIR>/snapshots/canonical`, sharing extents with the source. Both paths must be on the same CoW filesystem: a `tmpfs` source or a cross-device copy fails closed instead of silently duplicating bytes.

---

## Where to Go Next

- [`snow.yml` modes & secrets](snow-modes.md) — mode merge rules, `disabled: true`, and the single `WorldOrigin` coordinate model.
- [Manifest reference](reference/manifest.md) — the two manifest schemas, their namespaces, and their validation rules.
- [CLI reference](reference/cli.md) — flags, JSON output, and the daemon-owned commands returning in `0.2.0`.
