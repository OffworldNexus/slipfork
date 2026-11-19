# Getting Started (`0.1.0`)

From a Git checkout to a forkable, running world in about ten minutes. The only unusual requirement is a Btrfs filesystem for the state directory.

```console
$ cargo install --path crates/slipfork-engine   # slipfork-world-init
$ cargo install --path crates/slipfork          # slipfork, slipforkd, git-remote-slipfork
$ sudo slipfork daemon install                  # :80 socket activation + AppArmor profile
$ cd /path/to/your/project
$ slipfork doctor && slipfork up
$ slipfork fork feat-billing
$ slipfork open
```

!!! warning "Btrfs state directory"
    `mkfs.btrfs` erases the device. Keep the state directory on real disk, never RAM-backed `/tmp` `tmpfs`, and mount it `user_subvol_rm_allowed,noatime` before the first `slipfork` command.

!!! info "Root is for setup only"
    Only `mkfs.btrfs`, `mount`, `chown`, `daemon install`, and `apparmor_parser` need root. Never run `slipfork` or `slipforkd` under `sudo`, or your worktrees and vault end up root-owned.

## How a boot works

```mermaid
sequenceDiagram
  participant You
  participant CLI as slipfork
  participant D as slipforkd
  participant K as Btrfs
  You->>CLI: slipfork up
  CLI->>D: boot world over daemon.sock
  D->>K: clone the sealed base subvolume
  D-->>CLI: origin http://acme.localhost
  CLI-->>You: world row, URL, next commands
```

## 1. One-time host setup

| Requirement | Why |
|---|---|
| Linux `x86_64`, Rust 1.94+, a non-bare Git repo | Builds the binaries; `fork` creates a worktree (a `.jj` checkout forks a Jujutsu workspace instead) |
| Btrfs state directory, owned by you | Every world and sealed snapshot is one unprivileged `$UID:$GID` subvolume |
| Docker with `buildx`, `unshare`, `ip` | Rootfs export and world namespaces |
| `curl` on first boot | `slipforkd` downloads the pinned static `crun` once, sha256-verified |
| Docker usable without `sudo` | The rootfs export runs as you |

Mount the state directory. Replace `/dev/<device>` with a free device and keep the path on disk:

```console
$ mkdir -p ~/.cache/slipfork
$ sudo mkfs.btrfs -L slipfork /dev/<device>
$ sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> ~/.cache/slipfork
$ sudo chown "$(id -u):$(id -g)" ~/.cache/slipfork
```

Install the units and the AppArmor profile once. For a `cargo install` layout, name the binary directory explicitly:

```console
$ sudo "$(command -v slipfork)" daemon install --bin-dir "$(dirname "$(command -v slipfork)")"
wrote     /etc/systemd/system/slipforkd.socket
wrote     /etc/systemd/system/slipforkd.service
wrote     /etc/apparmor.d/slipfork
enabled   slipforkd.socket + slipforkd.service, ingress 127.0.0.1:80 + [::1]:80
next      slipfork up
```

Ubuntu 24.04 LTS restricts unprivileged user namespaces by default. `daemon install` writes and loads `/etc/apparmor.d/slipfork`, so `doctor` passes `unshare_userns` and `netns` afterwards.

Re-run the same command after moving the binaries; it rewrites only files whose bytes changed.

## 2. Probe the host

```console
$ slipfork doctor
state_dir: /home/acme/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
ingress: active :80
✓ Ready
```

`doctor` exits `0` only when the Btrfs checks and both namespace probes pass. On a non-Btrfs path it exits `1`, prints one `fix:` line per failed check, and writes nothing. `slipfork doctor --json` emits the same report as one object.

## 3. Declare the world in `snow.yml`

A world needs at least one build image with a named final Dockerfile stage and a `modes.local` entry:

```yaml title="snow.yml"
images:
  - { id: api, context: api }
components:
  - id: api
    image: api
    run_command: uv run uvicorn app.main:app --host 0.0.0.0 --port 8000
    envs: [app]
    internal_ports: [8000]
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
  local:
    envs:
      app:
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
```

`run_command` goes to `sh -eu -c` verbatim, so it is not templated: keep the literal port that `ingresses` declares. Expressions belong in `envs` values.

## 4. Boot and fork

```console
$ slipfork up
$ slipfork fork feat-billing
$ slipfork list
```

`up` prints one world row, the app URL, the worktree, and the next commands. `fork` creates `.worktrees/feat-billing` and clones the world's subvolume in one daemon call. Each fork gets its own origin, database, Redis, and S3 state.

!!! info "Jujutsu checkouts"
    If the checkout carries `.jj`, `fork` runs `jj workspace add` and projects the working copy into the colocated Git store. `jj` must be on `PATH`; Slipfork fails closed rather than silently using Git.

## 5. Work inside the world

```console
$ slipfork exec -- env | grep DATABASE_URL
$ slipfork shell
$ slipfork logs api --follow
$ slipfork db appdb
$ slipfork redis cache
$ slipfork ps
```

All of these resolve the world from `cwd` or `--world` and need the world running. `db` and `redis` fail closed when the container is stopped; `forward` bridges a service port to an ephemeral host port for non-interactive tools.

## 6. Secrets: local first, CI when paired

`!!` generates a value once and keeps it in the project's `0600` vault at `~/.local/share/slipfork/vaults/<project>.json`. That tier is yours: app secrets, self-run database passwords, and local values that must survive a restart.

CI values are the other tier. Pair this machine once with `slipfork auth`; afterwards `<state_dir>/ospp.json` remembers the endpoint, account, and machine, so later daemons reconnect without the flag. Details: [Secrets & OSPP v1](reference/ospp.md).

## 7. Stop and clean up

```console
$ slipfork down --world feat-billing   # stop services, keep .worktrees/feat-billing
$ slipfork drop feat-billing           # remove world, worktree, branch, snapshot ref
$ slipfork prune                       # preview worlds whose checkout is gone
$ slipfork prune --yes                 # reap them (sealed snapshots and caches stay)
```

Next: [`snow.yml` modes & secrets](snow-modes.md), then the [CLI reference](reference/cli.md).
