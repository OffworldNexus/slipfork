# Getting Started (`0.1.0`)

Go from a repository to a running, isolated development world in four `slipfork` commands after installing and the one-time host setup below — zero `root:root` files, and root only for the Btrfs mount plus the Ubuntu 24.04 AppArmor profile.

```console
$ git clone https://github.com/OffworldNexus/slipfork && cd slipfork
$ cargo install --path crates/slipfork-engine    # installs `slipfork-world-init`
$ cargo install --path crates/slipfork           # `slipfork` + `slipforkd` + `git-remote-slipfork`
$ cd /path/to/your/project
$ slipfork doctor
$ slipfork snow resolve --file snow.yml --mode local
$ slipfork up
$ slipfork list
```

Every world boots from a read-only rootfs built from your own `Dockerfile`, so `snow.yml` needs at least one build image with a **named final stage** and a `modes.local` entry. Create `api/Dockerfile` with `FROM <image> AS app`; if `up` reports `snow.yml declares no buildable Dockerfile under 'images:'`, add an `images:` entry (`id`, `context`, `dockerfile`); if it reports `mode 'local' is not defined`, add `modes: { local: {} }`.

## Prerequisites

| Requirement | Why |
|---|---|
| **Linux `x86_64`**, **Rust 1.94+** (`edition = "2024"`), a non-bare `git` repo | Builds the CLI and `slipforkd`; `fork` creates a worktree (a `.jj` checkout resolves through its Git store). The pinned `crun` asset exists for `x86_64` only |
| **Btrfs** for the state directory, mounted `user_subvol_rm_allowed,noatime`, owned by you | Every world and sealed snapshot is one unprivileged `$UID:$GID` subvolume |
| Docker with `buildx`, plus `unshare` (util-linux) and `ip` (iproute2) | Rootfs export via `buildx build --output type=local`; world user/network/IPC/PID namespaces |
| `curl` + network on first boot | `slipforkd` downloads the pinned static `crun` once (sha256-verified, offline afterwards) into `<state_dir>/cache/runtime/` and stages it inside the world rootfs; `slipforkd --crun <PATH>` skips the download |
| Docker usable as your own user (`docker ps` needs no `sudo`) | Rootfs export runs as you. **Never** run `slipfork`/`slipforkd` under `sudo` — only `mkfs.btrfs`, `mount`, `chown`, and `apparmor_parser` need root, or `.worktrees/` and the vault end up root-owned |

One-time setup of the state directory, as root (`mkfs.btrfs` erases that device); keep it on disk, never RAM-backed `/tmp` `tmpfs`, and substitute your `--state-dir` path if you set one:

```console
$ mkdir -p ~/.cache/slipfork
$ sudo mkfs.btrfs -L slipfork /dev/<device>
$ sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> ~/.cache/slipfork
$ sudo chown "$(id -u):$(id -g)" ~/.cache/slipfork  # the fresh mount root is root-owned
```

## 1. Probe the host

```console
$ slipfork doctor
state_dir: /home/acme/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: missing
unshare_userns: ok
netns: ok
fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
```

That is the fail-closed path on a non-Btrfs state directory: `doctor` exits `1` and prints one `fix:` line per failed check — here a single mount fix (replace `<device>` with your Btrfs device and `<state_dir>` with the `state_dir:` line above). Each line is a probe, not a config echo:

| Line | Evidence |
|---|---|
| `state_dir`, `filesystem` | Requested state root and the `statfs` filesystem label (`btrfs`, `xfs`, `ext4`, `tmpfs`, `overlayfs`, `unknown(0x…)`) |
| `btrfs_super_magic` | `ok` only for `BTRFS_SUPER_MAGIC` (`0x9123683e`) |
| `subvolume_ioctl` | Live scratch round-trip (create → read-only snapshot → clear read-only → destroy) proving unprivileged `user_subvol_rm_allowed`; `skipped` off Btrfs, `failed (errno <n>)` otherwise |
| `unshare_userns`, `netns` | `unshare --user --map-user=<uid> --map-group=<gid>` succeeds, then `--user --net --ipc --pid` brings loopback up (`netns` is `skipped` when `unshare_userns` is not `ok`) |

On a Btrfs host those lines report `btrfs`, `ok`, `ok`, and `ok`, `fix:` disappears, and `doctor` exits `0`; `slipfork doctor --json` emits the same report as one object (field by field in the [CLI reference](reference/cli.md)). On Ubuntu 24.04, install the AppArmor profile in [§6](#6-ubuntu-2404-apparmor-required-on-stock-kernels-and-the-optional-systemd-daemon) before the namespace probes can pass.

`slipforkd` refuses to boot on a non-Btrfs state directory too, so no world command leaves a half-created tree:

```console
$ tail -1 "$XDG_RUNTIME_DIR/slipfork/daemon.log"   # <state_dir>/run/daemon.log when XDG_RUNTIME_DIR is unset
slipforkd: cannot start the Btrfs world engine: /home/acme/.cache/slipfork is not on Btrfs: statfs reports f_type 0x58465342, not BTRFS_SUPER_MAGIC 0x9123683e; fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
```

Every world command then exits with `slipfork: slipforkd exited with exit status: 1 before serving …; see …/daemon.log`.

## 2. Resolve `snow.yml`

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
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
```

```console
$ slipfork snow resolve --file snow.yml --mode local
DATABASE_URL=postgres://127.0.0.1:5432/appdb
LOG_LEVEL=INFO
SECRET_KEY=[REDACTED]
```

`!!` fills a missing secret once and persists it in a `0600` vault; re-running after a manifest edit keeps the value. A top-level `envs:` or `project:` block **in `snow.yml`** is refused by key name only (`project` is a `slipfork.toml` key). Components, hooks, ingresses, and every key: [`snow.yml` modes & secrets](snow-modes.md).

## 3. Boot the canonical world

```console
$ slipfork up
[resources] appdb: prefer_remote -> local (not_offered)
[ready]     api (internal :8000 in world netns)
[origin]    http://acme.localhost:41921
```

`not_offered` means no paired OSPP session offered a remote database, so `appdb` ran world-local. The first `up` builds and caches the rootfs under `<state_dir>/cache/rootfs/<sha256>/app`, seals it as a read-only Btrfs snapshot, clones the world's writable subvolume from it, starts one `crun` OCI container per declared resource sharing the world's `127.0.0.1`, and runs your components as `$UID:$GID` on the read-only rootfs. Repeating `up` prints the same origin URL.

`slipfork` auto-starts `slipforkd` when `<runtime_dir>/daemon.sock` does not answer (`<runtime_dir>/daemon.log`, `0600`); `HOME` must be set before any mutation. Host toolchain caches: [CLI reference](reference/cli.md) (`--host-caches`).

## 4. Fork a second world

```console
$ slipfork fork feat-billing
[origin]    http://feat-billing.acme.localhost:41921   # after the same [resources]/[ready] rows

$ slipfork down --world feat-billing   # stop services, keep .worktrees/feat-billing
$ slipfork drop feat-billing           # destroy the world, worktree, branch, and snapshot ref
```

A fork is a Git worktree (`.worktrees/<fork>` + `refs/slipfork/<fork>/wip`) plus its own Btrfs subvolume cloned by one subvolume snapshot, created in one daemon call. If the checkout carries `.jj`, the fork is a Jujutsu workspace instead: `slipfork fork` runs `jj workspace add`, projects `@-`/`@` into the colocated Git store, and fails closed with `ToolMissing` when `jj` is not on `PATH` — never a silent Git fallback.

## 5. Work inside the running world

All of these resolve the world from `cwd` or `--world`, and need the world running:

```console
$ slipfork db appdb                          # psql inside the postgres:<v> container (TTY)
$ slipfork redis cache                       # redis-cli inside the redis:<v> container (TTY)
$ slipfork logs api                          # retained component output; --follow streams
$ slipfork forward pg_databases.appdb        # 127.0.0.1:<ephemeral> bridged into the world
```

`db`/`redis` fail closed with `ServiceNotRunning` when that container is stopped; non-interactive agents can run `slipfork forward <target>` in the background and connect with host `psql` instead.

## 6. Ubuntu 24.04 AppArmor (required on stock kernels), and the optional systemd daemon

Stock Ubuntu 24.04 LTS kernels set `kernel.apparmor_restrict_unprivileged_userns=1`, so unprivileged user namespaces are denied until the `slipfork` AppArmor profile is installed — the second root step here (the optional systemd units below are the third). `doctor` reports `unshare_userns: failed` with the hint and `netns: skipped`:

```console
$ slipfork daemon unit --only apparmor | sudo tee /etc/apparmor.d/slipfork
$ sudo apparmor_parser -r /etc/apparmor.d/slipfork
```

The profile must name **every** executable that calls `unshare(CLONE_NEWUSER)` — the `slipfork`/`slipforkd`/`slipfork-world-init` entrypoints, `/usr/bin/unshare`, and the in-world `crun` — because an unnamed caller is still denied. On a dedicated development VM you can instead switch the restriction off for the whole guest (prefer the profile on a shared host):

```console
$ echo 'kernel.apparmor_restrict_unprivileged_userns=0' | sudo tee /etc/sysctl.d/99-slipfork-userns.conf
$ sudo sysctl --system
```

`slipfork daemon unit --only socket` / `--only service` are **optional**: they render the units to stdout with zero filesystem writes, embedding the absolute binary, state, and runtime paths; the socket unit binds `127.0.0.1:80`, `[::1]:80`, and `127.0.0.1:7080`. Install them with `slipfork daemon unit --only socket | sudo tee /etc/systemd/system/slipforkd.socket` and `--only service | sudo tee /etc/systemd/system/slipforkd.service`; the auto-spawn path above needs no `sudo`. Render from the installed binaries, not a build tree; flags: [CLI reference](reference/cli.md#slipfork-daemon-unit).

Next: [`snow.yml` modes & secrets](snow-modes.md), [orthogonal DAG planner](testing-dag.md), [manifest reference](reference/manifest.md).
