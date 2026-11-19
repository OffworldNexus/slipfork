# DigitalOcean Dev-VM Harness (`tools/do_vm.py`)

- **Status:** Internal operator runbook for the shipped harness. **Not a supported deployment path.**
- **Visibility:** Internal — `docs/design/` is excluded from the published Zensical site.

Slipfork needs a real Btrfs filesystem plus unprivileged `userns`/`netns`/`ipcns`/`pidns`/`mntns`, `pivot_root`, and a static `crun` inside the world rootfs. A workstation whose root filesystem is XFS cannot prove that.

This harness boots one disposable Ubuntu 24.04 droplet, attaches a block-storage volume formatted **Btrfs** at `/var/lib/slipfork`, provisions the toolchain, syncs the working tree, and runs the real-kernel suites as an unprivileged user. It installs no host `crun`.

## Quickstart

```console
$ doctl auth init                 # once: the harness shells out to an authenticated doctl
$ tools/do_vm.py up               # droplet + Btrfs volume + packages
$ tools/do_vm.py sync             # build, install, daemon install, seed the demo
$ tools/do_vm.py test             # real-kernel suites
$ tools/do_vm.py ssh -- slipfork status
$ tools/do_vm.py down --yes       # the only destructive command
```

## Commands

| Command | Job |
|---|---|
| `up` | Create or reuse the tagged droplet and volume, format a blank volume Btrfs, provision packages and Rust, write the login banner |
| `status` | Print the droplet, volume, addresses, and readiness without changing anything |
| `sync` | Stream the working tree, build the product binaries, install them, run `slipfork daemon install`, seed the demo once |
| `test` | Run the real-kernel suites as the unprivileged account |
| `ssh` | Open a root session, or forward one command after `--` |
| `down` | Destroy the droplet; keeps the volume unless `--delete-volume` |

`up` flags: `--region` (default `ams3`), `--size` (default `s-4vcpu-8gb`), `--volume-gb` (default `100`, minimum `10`), repeatable `--identity`, repeatable `--authorized-key`, `--format-blank`, `--yes`.

`up` is idempotent by the tag `slipfork-dev-vm`. An existing tagged droplet wins, so `--size` and `--volume-gb` apply only to a new one.

## What `up` provisions

- One droplet from `ubuntu-24-04-x64` and one block-storage volume mounted at `/var/lib/slipfork` with `user_subvol_rm_allowed,noatime`.
- `btrfs-progs`, `docker.io`, `docker-buildx`, `openssh-server`, `apparmor`, `curl`, `ca-certificates`, `build-essential`, and `git`.
- A pinned `rustup-init` (SHA-256 verified) and the `slipfork` account, whose `~/.cache` is linked onto the Btrfs volume so heavy state never lands on the root disk.
- Authorized keys for the default identity and every `--authorized-key`.

The harness installs no host `crun`. The pinned static binary is downloaded by `slipforkd` into product state and staged inside each world rootfs, exactly as on any other host.

!!! warning "Volume safety"
    A device with a foreign filesystem fails closed even with `--format-blank`. A pre-existing blank volume is formatted only when the first 8 MiB read back as all zeros. `up` never reformats a volume it did not create.

## The identity

The default bootstrap key is `~/.cache/slipfork/do-vm/id_ed25519`, materialized by `up` when absent. `status`, `sync`, `test`, and `ssh` never generate it; when it is missing they fail closed and point at `up`.

Every SSH call uses a fixed, config-free option set: `-F /dev/null`, `IdentitiesOnly=yes`, `IdentityAgent=none`, no control socket, and a per-droplet `known_hosts` under `<worktree>/.slipfork/do-vm/`. Ambient keys, agents, and jump hosts cannot leak in.

The key is operator credential state, not product state. It sits outside the `worlds/`, `snapshots/`, `artifacts/`, and `cache/` trees that `slipforkd` enumerates or `prune` reaps.

## `sync` is also the install step

`sync` streams the checkout to `/home/slipfork/slipfork`, builds the four product binaries in the `debug` profile, and installs them into `/usr/local/bin`.

It then runs the product's own installer, which is the single writer of the units and the single owner of the socket handover:

```console
$ slipfork daemon install --user slipfork \
    --bin-dir /usr/local/bin --bin-dir /home/slipfork/slipfork/target/{debug,release}
```

Finally it asserts `systemctl is-active slipforkd.socket`. On a first run it also seeds `support/demo` at `/home/slipfork/demo` as one Git commit. Later syncs never overwrite a seeded demo.

Both `sync` and `test` stop the installed units and take the control socket from any running `slipforkd`, so running worlds stop until the next `slipfork up`. A missing product binary fails closed and names the remedy.

## `test`

`test` runs the real-kernel suites inside the VM as the unprivileged `slipfork` account and refuses to run if `slipfork` resolves to uid 0. Suites cover the live Btrfs engine, world lifecycle, OSPP pairing against a loopback server, and the CLI.

The harness is deliberately not a deployment: no backups, no HA, no firewall hardening, and no secret management beyond the bootstrap key. Keep product state under `/var/lib/slipfork` and the harness identity where it is.
