# Getting Started (`0.2.0`)

Build `slipfork`, prove your host can do copy-on-write and unprivileged namespaces, resolve a `snow.yml` manifest, plan a build/fixture DAG, and boot a live world through the zero-setup `slipforkd` daemon — with zero `sudo` and zero `root:root` files.

```console
$ cargo install --path crates/slipfork
$ slipfork doctor
$ slipfork snow resolve --file snow.yml --mode local
$ slipfork dag plan --file slipfork.toml --repo .
$ slipfork up
$ slipfork list
```

!!! info "World lifecycle is daemon-served"
    `slipfork up`, `down`, `fork`, `drop`, `list`, `prune`, `share`, `secret`, `exec`, and `daemon unit` drive the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock`; the CLI auto-spawns `slipforkd --listen 127.0.0.1:0` when the socket is absent. `doctor`, `snow resolve`, and `dag plan` run locally. Interactive `logs`, `shell`, `db`, `redis`, `forward`, `hook run`, `auth login`, `support`, and `git-remote-slipfork` land in the remainder of `0.2.0`.

---

## 1. Build & Probe the Host

Prerequisites: **Linux** (`x86_64` / `aarch64`), **Rust 1.94+** (`edition = "2024"`), a non-bare `git` repository (a colocated Jujutsu checkout resolves through its Git store), `unshare` / `ip` (`util-linux` + `iproute2`), and a CoW filesystem (`XFS` with `reflink=1`, `Btrfs`, or `ZFS`) under `~/.cache/slipfork` — never RAM-backed `/tmp` `tmpfs`.

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
    `<project>` is your canonical `git` root's directory name (linked `.worktrees/<fork>` resolves back to the main repository through Git). Git submodules and `--separate-git-dir` layouts fail closed until you set `project = "<slug>"` in `slipfork.toml`, and a bare repository has no working tree to develop in — clone it first. Jujutsu-only workspaces without a colocated Git store fail closed today; a `jj git init --colocate` checkout resolves through its Git store, and jj-aware resolution lands later in `0.2.0`.

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

## 4. Boot a World Through `slipforkd`

`slipfork up` boots the canonical world for the current repository. The world is cloned from a CoW base snapshot, mounted over your checkout, attached to the daemon's `.localhost` ingress, and started as supervised `WorldProcess` components; the command prints exactly one origin URL.

```console
$ slipfork up
[resources] appdb: prefer_remote -> local (not_offered)
[origin]    http://acme.localhost:41231

$ slipfork list
WORLD            STATE     ORIGIN                                 WORKTREE
acme             running   http://acme.localhost:41231            /home/acme/acme
```

A fork is a second CoW world plus a `.worktrees/<fork>` checkout, created in one daemon call:

```console
$ slipfork fork feat-billing
[origin]    http://feat-billing.acme.localhost:41231

$ slipfork exec --world feat-billing -- true
$ slipfork down --world feat-billing      # stop services, keep .worktrees/feat-billing
$ slipfork drop feat-billing              # destroy the world and the worktree
$ slipfork prune --dry-run                # preview orphaned worlds; --yes reaps them
```

- **Zero setup:** when `<runtime_dir>/daemon.sock` does not answer, `slipfork` starts `slipforkd --listen 127.0.0.1:0 --state-dir <state> --runtime-dir <runtime>` in the background and records its diagnostics in `<runtime_dir>/daemon.log` (`0600`). Set `--state-dir`/`--runtime-dir` (or `$XDG_CACHE_HOME`/`$XDG_RUNTIME_DIR`) to relocate them; `HOME` must be set before any mutation.
- **One URL per world:** `list` prints one `WorldOrigin` per line (`http://<host>` on `:80`, otherwise `http://<host>:<port>`). `--json` emits the same rows as objects.
- **Sharing fails closed:** `slipfork share` requires a bound forward listener and the `weaved` tunnel adapter; both are absent today, so it exits non-zero and tells you to use the world's `.localhost` origin. `slipfork share --stop` is an idempotent no-op.
- **Secrets never echo:** pipe values on stdin (`printf '%s' "$VALUE" | slipfork secret set STRIPE_SECRET_KEY`), rotate with `--force`, list key names only, and remove with `slipfork secret unset STRIPE_SECRET_KEY`. The local vault is `0600` at `~/.local/share/slipfork/vaults/<project>.json`.
- **Explicit systemd units (optional):** `slipfork daemon unit --only socket` and `--only service` render non-template units to stdout with `User=<your-account>` and the explicit `--state-dir`/`--runtime-dir`; the CLI writes no files. Without them, auto-spawn is enough for laptops, CI, and agents.
- **Current component runtime:** `0.2.0` starts `WorldProcess` components from the host toolchain. The Dockerfile-derived read-only `L0` rootfs and `crun` backing-service containers land in the remainder of M2; the world lifecycle, single origin, and CoW state described here are already shipped.

---

## Where to Go Next

- [`snow.yml` modes & secrets](snow-modes.md) — mode merge rules, `disabled: true`, and the single `WorldOrigin` coordinate model.
- [Manifest reference](reference/manifest.md) — the two manifest schemas, their namespaces, and their validation rules.
- [CLI reference](reference/cli.md) — flags, JSON output, daemon-served lifecycle, secrets, and systemd units.
