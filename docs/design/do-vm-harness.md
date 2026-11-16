# DigitalOcean Dev-VM Harness (`tools/do_vm.py`)

- **Status:** Internal design / operator runbook (`M3.5`).
- **Visibility:** Internal (`docs/design/` is excluded from the published Zensical site).
- **Why:** The development host has no `/var/lib/incus/unix.socket` (`H-004`), so the live `IncusEngine` conformance suite cannot run there. This harness provisions a disposable DigitalOcean droplet with a block-storage volume formatted as ZFS, installs Incus, registers that ZFS pool as Incus storage `default`, and syncs the working tree into `/root/slipfork`.

---

## 1. Commands (exactly six)

```console
tools/do_vm.py up      [--region ams3] [--size s-4vcpu-8gb] [--volume-gb 100]
                       [--ssh-key NAME_OR_ID ...] [--authorized-key PATH ...] [--yes]
tools/do_vm.py status
tools/do_vm.py sync
tools/do_vm.py test    [-- <extra cargo args>]
tools/do_vm.py ssh
tools/do_vm.py down    [--yes]
```

Defaults: region `ams3`, size `s-4vcpu-8gb` (4 vCPU / 8 GiB / 160 GiB SSD), volume `100 GiB`, image `ubuntu-24-04-x64`. `up` is idempotent by the tag `slipfork-dev-vm`; it **never destroys anything**. Teardown is always the separate `down --yes`.

### `up`

1. Resolve every resource tagged `slipfork-dev-vm` (droplet and volume) **independently** and find-or-create each one. A complete pair is reused; a droplet-only or volume-only state is reconciled rather than duplicated:
   - **volume-only:** create the droplet with the existing volume attached (`--volumes <id>`); cloud-init provisions it.
   - **droplet-only:** create the tagged volume, attach it only when the droplet's `volume_ids` does not already contain it (`doctl compute volume-action attach ... --wait`), then re-run the idempotent provisioning script over SSH.
   - **both, but not attached:** attach once, then re-provision.
   - **both, attached:** a second `up` creates and attaches nothing; it is a no-op report (plus readiness/fingerprint probes).
   Resources in another region are never silently reused.
2. Validate the requested region against `doctl compute region list` and the size against `doctl compute size list` (the latter also supplies the droplet's `price_monthly`).
3. Create the volume first (`--tag slipfork-dev-vm`, `--size 100GiB`, same region), then create the droplet with `--volumes <volume-id>` so the raw block device is attached at first boot. `--wait --wait-timeout 10m` bounds droplet activation; volume attaches use the same bound.
4. Wait (bounded to 15 minutes) for `/root/incus-ready`. If the droplet pre-dates a late volume attach, or cloud-init has finished without the marker, a separate bounded (5-minute) SSH-reachability gate runs before re-provisioning; authentication rejections (`Permission denied (publickey)`) fail fast with `--ssh-key` / `--authorized-key` guidance, while other SSH wording keeps polling. The same idempotent provisioning script then runs over SSH stdin. On timeout, print the tail of `/var/log/cloud-init-output.log` and `journalctl -u cloud-init` before failing.
5. Install `--authorized-key` files, then print the summary (IP, `ssh root@IP`, monthly cost, exact `down --yes`) **before** the fingerprint probe; a fingerprint failure still prints the teardown command. Without `--authorized-key`, `up` still succeeds with a warning.
6. Print the installed `--authorized-key` fingerprints.

### `status`

Prints the tagged droplet/volume state plus the remote `/root/.ssh/authorized_keys` fingerprints read over SSH. If the SSH probe (including host-key verification) cannot run, `status` exits non-zero rather than reporting a stale or assumed state. With no tagged droplet it prints `none` and exits 0.

### `sync`

Runs `git ls-files --cached --others` plus `--exclude-standard` on the working tree, so tracked **and** uncommitted non-ignored files are sent while `.gitignore` is respected. `target/`, `site/`, and `.worktrees/` are always pruned. The file list is streamed as a tar archive into `ssh root@IP 'tar -x -C /root/slipfork'` using Python's `tarfile` (no `rsync`, no local shell pipeline). `.env`-class files (`.env`, `.env.*`, except `.env.example`) are refused before any SSH call, but only when they are selected for transfer: gitignore-excluded files are neither sent nor refused. It never runs `git add` or commits.

### `test`

Pre-checks the local `crates/slipfork-engine/tests/critical_incus_live.rs` (the `M3.3` deliverable) and then the remote copy under `/root/slipfork`, failing closed with the missing path and `M3.3` when absent. It also requires `cargo` on the VM: **before the first `test`, install Rust >= 1.85 (edition 2024) with `rustup` on the VM** — cloud-init intentionally provisions ZFS + Incus only, and `up` prints this as a `prereq` line. It streams:

```console
cargo test -p slipfork-engine --test critical_incus_live -- --test-threads=1 [extra args]
```

and exits with its status.

### `ssh`

Resolves the single tagged droplet and `exec`s an interactive `ssh root@IP` session.

### `down`

`--yes`-guarded, tag-scoped deletion: all tagged droplets first, then all tagged volumes. Non-tagged resources are never touched. Volumes are deliberately raw and disposable; their ZFS pool has no backup path.

---

## 2. Cloud-init provisioning

A single `runcmd` chain (`bash -lc`, `set -euo pipefail`), also kept as the idempotent script that `up` re-runs over SSH stdin when reconciling a late volume attach:

1. `apt-get update`, then check `apt-cache policy incus`; if the candidate is missing, print the exact policy output and stop. **No PPA, no third-party repo, no zabbly fallback.**
2. `apt-get install -y --no-install-recommends zfsutils-linux incus`; on failure, print the same policy output and stop.
3. If `zpool list incuspool` already succeeds, keep it; otherwise wait (bounded) for exactly one `/dev/disk/by-id/scsi-0DO_Volume_*` device and run `zpool create -o ashift=12 -O mountpoint=/var/lib/incus/disks/pool incuspool /dev/disk/by-id/scsi-0DO_Volume_*`.
4. `systemctl enable --now incus`, then wait (bounded) for `incus info` to answer. The daemon must be running before storage registration, so the enable/wait step precedes `incus storage create`.
5. If `incus storage show default` already succeeds, keep it; otherwise `incus storage create default zfs source=incuspool`. Then write `/root/incus-ready`.

Every step is safe to repeat, so a re-run of `up` after an interrupted cloud-init converges instead of erroring or creating a second pair.

---

## 3. Cost model

The droplet price is read from `doctl compute size list` (`price_monthly`), never hardcoded. Block storage uses the DigitalOcean list price as of 2026-11-16, `$0.10/GiB-month` — re-verify before `up`. For the defaults in `ams3`:

| Resource | Monthly |
|---|---|
| Droplet `s-4vcpu-8gb` (4 vCPU, 8 GiB, 160 GiB SSD) | `$48.00` |
| Volume `100 GiB` | `$10.00` |
| **Total while `up`** | **`$58.00/month`** |

`up` prints the computed droplet + volume line before asking for confirmation. The volume exists (and bills) until `down --yes`; `up` itself never removes it.

---

## 4. Why ZFS, and why LXC only

The Incus `zfs` storage pool on the block volume gives the engine `ioctl(FICLONE)`-class copy-on-write clones: `IncusEngine` copies instance snapshots with `source.type = "copy"`, so a fork is a metadata operation over reflinked ZFS blocks rather than a full filesystem copy. This is the same CoW premise as the content-addressed artifact/fixture snapshot DAG (`docs/design/world-base-image.md` §6) and the reason the harness provisions ZFS instead of ext4/xfs. `ashift=12` matches the 4 KiB sector geometry DigitalOcean exposes.

DigitalOcean droplets **do not expose nested KVM** (`/dev/kvm` is absent), so Incus can only supervise **system containers (LXC)** there. The harness therefore verifies the container path of `IncusEngine`; QEMU VMs, stateful RAM snapshots, and any claim of full-machine isolation are explicitly out of scope on DigitalOcean. Nothing in this harness or in published docs should claim otherwise.

---

## 5. SSH keys: the DO registry quirk and `--authorized-key`

Observed on this account: a key can be registered on the DigitalOcean account yet be invisible to the current `doctl` context. In that state `doctl compute ssh-key import` was observed to fail with `422 ... SSH Key is already in use on your account`, while `doctl compute ssh-key list` showed a different key. The registry therefore cannot guarantee that a particular key lands on a fresh droplet.

The harness handles this in two layers:

1. **Create-time embedding:** by default `up` passes **every** key id visible to the current context as `--ssh-keys id1,id2,...`. Zero visible keys is a fail-closed error. Repeatable `--ssh-key NAME_OR_ID` restricts the set; each value is resolved against the visible list and unknown values fail closed. There is no implicit "pick the only key" behavior.
2. **Post-boot guarantee:** repeatable `--authorized-key PATH` reads each local public-key file at runtime and appends it over SSH stdin to `/root/.ssh/authorized_keys` (`cat >>` then `sort -u -o`), so a second identical `up` leaves the file byte-identical. Key material never appears in command argv, logs, this document, or the cloud-init script. The resulting `ssh-keygen -lf /root/.ssh/authorized_keys` fingerprints are printed as evidence.

SSH authentication rejections (`Permission denied (publickey)`, `Too many authentication failures`) fail fast during readiness and `test` probes with `--ssh-key` / `--authorized-key` guidance, instead of burning the cloud-init timeout or reporting "cannot reach".

---

## 6. Explicit non-goals

- **Not a supported deployment path.** No backups, no HA, no firewall hardening, no production secrets, no public documentation, no release artifact.
- Not a benchmark target: this VM exists to run live Incus conformance tests and to let the owner log in and poke the provisioned Incus/ZFS stack.
- No stateful/QEMU Incus coverage (no nested KVM on DigitalOcean).
- No automatic teardown: cost control is an explicit human `down --yes`.
