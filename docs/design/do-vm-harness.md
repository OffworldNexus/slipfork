# DigitalOcean Btrfs Dev-VM Harness (`tools/do_vm.py`)

- **Status:** Internal design / operator runbook (design of record for `M3.10` -> `M3.16`).
  Target state — `tools/do_vm.py` is rewritten by `M3.16`; the shipped tool is still the
  retired harness, so do not run this runbook against today's binary.
- **Visibility:** Internal (`docs/design/` is excluded from the published Zensical site).
- **Why:** `KernelBtrfsDriver` fails closed with `EngineError::BtrfsRequired` unless
  `statfs(state_dir).f_type == 0x9123683e`, and `BtrfsEngine` needs real Linux
  `userns`/`netns`/`ipcns`/`pidns`/`mntns`, `pivot_root`, and a static `crun` inside the world
  rootfs, on the Ubuntu 24.04 target (`ADR-0010` §§4-7). A development host whose root
  filesystem is XFS cannot provide that. This harness boots one disposable DigitalOcean
  droplet, attaches a block-storage volume formatted **Btrfs** at `/var/lib/slipfork`,
  provisions `btrfs-progs`, `docker.io`/`docker-buildx`, a Rust toolchain, and the
  `/etc/apparmor.d/slipfork` `userns` profile, syncs the working tree, and streams the
  real-kernel suites **as an unprivileged user**. It installs **no host `crun`** (§2 step 1).

---

## 1. Commands (exactly six)

```console
tools/do_vm.py up      [--region ams3] [--size s-4vcpu-8gb] [--volume-gb 100]
                       [--identity PATH ...] [--yes]
tools/do_vm.py status  [--identity PATH ...]
tools/do_vm.py sync
tools/do_vm.py test    [--identity PATH ...]
tools/do_vm.py ssh     [--identity PATH ...] [-- <remote command> ...]
tools/do_vm.py down    [--yes]
```

Defaults: region `ams3`, size `s-4vcpu-8gb` (4 vCPU / 8 GiB / 160 GiB SSD), volume
`100 GiB`, image `ubuntu-24-04-x64`. `up` is idempotent by the tag `slipfork-dev-vm`; it
never deletes a droplet or volume and never reformats a device that carries any filesystem.
The only destructive action is `mkfs.btrfs` on a device proven **empty** (§2 step 4). Teardown is
always the separate `down --yes`.

### `--identity`: one persistent bootstrap key surface

**Ownership — harness credential state, not product state.** The default identity is the
owner-specified absolute path `~/.cache/slipfork/do-vm/id_ed25519`, resolved by
`tools/do_vm.py` from the operator's own home directory (`$HOME` of the process running the
tool) and from nothing else. The path sits under the default product cache directory only
because the owner specified that absolute path: it is **not** derived from the product's
`Layout::resolve`, `$XDG_CACHE_HOME`, `$XDG_DATA_HOME`, `--state-dir`, or `--runtime-dir`, so
changing any of those leaves it exactly where it is. It is not product state either: it lives
in `do-vm/`, outside the `worlds/`, `snapshots/`, `artifacts/`, and `cache/` trees that
`slipforkd` enumerates or `prune` reaps, so the product never reads, writes, or deletes it.
That exclusion is a rule, not a structural guarantee: because the absolute path sits inside the
default state root, any future state-root GC (`M3.2.13`) **MUST** exclude `do-vm/`; the
structural alternative — a path outside `<state_dir>` entirely — is the owner's to choose and
has not been chosen.
`tools/do_vm.py` is a standalone stdlib-only operator tool (the `# /// script` header), not a
Slipfork crate, and this is its single boundary that reads the operator's home; `sync` never
ships the key (it is outside the repo) and `down` never deletes it.

`up` always materializes the default identity when absent (`mkdir -p -m 700` the parent,
`ssh-keygen -t ed25519 -N '' -C slipfork-do-vm -f <path>`), resolves its public-key body
against the DigitalOcean account, and embeds it at create time. It is therefore the transport key for
`up`/`sync` and the reboot-safe way back into a running droplet.

**Per-command semantics** (identical flags, different jobs):

| Command | `--identity` | Behaviour |
|---|---|---|
| `up` | repeatable | Default identity materialized first and used as the create-time bootstrap and account-registry key (§5); every listed public key is appended after boot to `/root/.ssh/authorized_keys` (`sort -u`, idempotent) |
| `status`, `test` | at most one; two or more is a usage error (exit 2) | The single listed path is the SSH transport identity (default when omitted); a listed key is never silently unused |
| `ssh` | at most one; two or more is a usage error (exit 2) | The single listed path is the SSH transport identity; everything after the single `--` is forwarded as the remote command |
| `sync` | none | Not API-only — it streams over SSH — but it takes no flag and always uses the default identity; it is only meaningful after `up` guaranteed it |
| `down` | none | API-only: opens no SSH at all and speaks only to the DigitalOcean API |

`status`, `test`, `ssh`, and `sync` never materialize or regenerate the default identity: when
it is absent they fail closed naming the path and pointing at `up`.

All SSH calls use one fixed, config-free option set — `-F /dev/null -o IdentitiesOnly=yes
-o IdentityAgent=none -o ControlMaster=no -o ControlPath=none -o StrictHostKeyChecking=accept-new
-o UserKnownHostsFile=<worktree>/.slipfork/do-vm/known_hosts-<droplet-id> -o ConnectTimeout=10
-i <path>` (plus `BatchMode=yes` and `ServerAliveInterval=30`/`ServerAliveCountMax=4` for
non-interactive calls) — so no ambient `~/.ssh` key, agent, jump host, control socket, or
`known_hosts` entry can leak in. `<worktree>/.slipfork/do-vm/` is created `0700` before the
first call, so `accept-new` can persist the pin. `accept-new` pins the droplet's host key on
first contact only; a changed key fails closed naming the file to inspect.

**Per-worktree `known_hosts` caveat.** `<worktree>` is the top level of the Git worktree the
command runs in, so the pinned host key is stored per worktree (`.gitignore`d under
`.slipfork/`). This is deliberate: the identity key is durable credential state shared by
every worktree, while the host-key pin is per-worktree so a pin never silently travels
between clones. A second worktree — or a fresh clone — therefore starts with an empty file and
`accept-new` pins the droplet's host key there without continuity with the first pin, and
`down` run from that other worktree cannot remove the first worktree's file. Run
`up`/`status`/`test`/`ssh`/`down` from the worktree that created the droplet whenever host-key
continuity matters, and never copy a `known_hosts` file between worktrees: a genuinely changed
host key must fail closed, not be hand-approved.

**The failure mode this exists for.** Harnesses usually run from a session whose key lives
under `/tmp`, which is RAM-backed `tmpfs`. Reboot the droplet (or end that session) and the
key is gone while the droplet keeps running and billing — the owner is locked out of a
machine only `doctl` can delete. The default key therefore lives on durable disk, survives
reboots, and is reused by every later `up`.

### `up`

1. **Classify only** (read-only): at most one tagged droplet and one tagged volume; more than
   one fails closed pointing at `down --yes`, and a tagged resource in another region fails
   closed naming both regions. Nothing is created in this step.
2. Materialize the default identity, then fail closed **before any create** if a listed
   private key or `.pub` is unreadable, if a `.pub` is missing and `ssh-keygen -y` cannot
   derive it, or if the default public key can neither be found nor imported (§5). A
   passphrase-protected private key is rejected here too: `ssh-keygen -y -P '' -f <path>` is
   run non-interactively and a non-zero status fails closed naming the path, because the fixed
   SSH option set has `IdentityAgent=none` and can never answer a prompt.
3. Validate `--region` against `doctl compute region list`, the size against
   `doctl compute size list` (including that the size is offered in that region), and
   `--volume-gb >= 10`.
4. Print the cost line — droplet price from `doctl compute size list` (`price_monthly` absent
   -> fail closed) plus the volume at the documented `$0.10/GiB-month` (§7) — and require
   confirmation; `--yes` skips it, and a non-interactive shell without `--yes` fails closed.
5. Create the missing volume first (tagged, same region), then the droplet with
   `--volumes <volume-id>` so the raw block device is attached at first boot; an existing
   droplet attaches the existing volume only when its `volume_ids` lacks it. Droplet
   creation is `--wait --wait-timeout 10m`; attaches use the same bound. A `cloud-init` that is
   still `running` is waited out, never interrupted; once it reports done, the bounded SSH gate
   below may re-run the idempotent provisioning script if the readiness marker is absent.
6. Wait (bounded to 15 minutes) for `/var/lib/slipfork/.slipfork-ready`, which lives inside
   the mount by construction (§2 step 7). On a droplet that is already reachable the wait
   returns at the first successful probe, so a marker-delete repair (§3) costs at most the
   5-minute gate below, not the full bound. A late attach or a failed probe opens a bounded
   (5-minute) SSH
   gate — authentication rejections are retried for a 90-second cold-boot grace, then fail
   with identity guidance — and then re-runs the idempotent provisioning script over
   `ssh 'bash -lc "bash -s"'`. On timeout, print the tail of
   `/var/log/cloud-init-output.log` and `journalctl -u cloud-init.service -u cloud-final.service`
   before failing.
7. Append every `--identity` public key to `/root/.ssh/authorized_keys` (`sort -u`,
   idempotent), print the summary (IP, `ssh root@IP`, Btrfs mount, monthly cost, exact
   `down --yes`) **before** the fingerprint probe, then print `ssh-keygen -lf` fingerprints.

Every Python-side bound (readiness wait, SSH gate, cold-boot grace, `down` poll) is a
module-level named constant patched by tests; the cloud-init script's own waits (dpkg lock,
device wait) are bounded shell loops rendered from the same constants. No sleep or timeout is
inline.

### `status`

Prints the tagged droplet/volume state (a volume-only state is reported as still billing)
plus the remote `/root/.ssh/authorized_keys` fingerprints read with the selected identity. If
the SSH probe (including host-key verification) cannot run, `status` exits non-zero rather
than reporting a stale or assumed state. With no tagged droplet it prints `none` and exits 0.

### `sync`

Runs `git ls-files --cached --others --exclude-standard` on the working tree, so tracked
**and** uncommitted non-ignored files are sent while `.gitignore` is respected. `target/`,
`site/`, and `.worktrees/` are always pruned. The archive is built in-process with Python's
`tarfile` (no `rsync`, no local shell pipeline) and streamed over SSH stdin, extracted
remotely with `ssh … 'tar -x -C /home/slipfork/.sync'` into a fresh staging tree, which then
replaces `/home/slipfork/slipfork` while preserving the remote `target/`; the result is
`chown -R slipfork:slipfork`'d. A file
deleted locally therefore cannot survive remotely and keep a deleted suite alive. `.env`-class
files (`.env`, `.env.*`, except `.env.example`) are refused before any SSH call, but only
when they are selected for transfer: gitignore-excluded files are neither sent nor refused.
It never runs `git add` or commits. Its remote `tar`/`chown` steps run as `root` under root's
own `HOME` (`/root`) by design: they write only `/home/slipfork/.sync` and
`/home/slipfork/slipfork` and read no `$HOME`-derived path, so there is no suite `HOME` to
contract and nothing caches off the volume.

### `test`

Pre-checks both local suites and fails closed naming both paths, the suite, and the node that
delivers each one:

```text
crates/slipfork-engine/tests/critical_btrfs_live.rs   (M3.14)
crates/slipfork/tests/critical_btrfs_live.rs          (M3.15)
```

It then preflights the droplet, each failure naming its own cause and exiting non-zero
before any suite runs:

1. The synced copies exist (else "run `tools/do_vm.py sync` first") and
   `test -f /var/lib/slipfork/.slipfork-ready` (placement and ordering: §2 step 7).
   An SSH transport failure (exit `255`) is reported as a transport failure, never as a suite
   failure.
2. `/home/slipfork/.cargo/bin/cargo --version` reports >= 1.94 (else "incomplete
   provisioning; re-run `up`"), and the suite prefix below with `cargo fetch --locked`
   succeeds (network or authenticated-remote failures are reported as such, never as a suite
   failure).
3. **One `HOME` contract.** Every suite-side remote command runs with
   `HOME=/var/lib/slipfork/home`, including the `docker info` socket probe — never bare
   `/home/slipfork`, which is ext4 and would let a suite cache its state off the volume. The
   owner `ssh` session has its own root-owned home (§1 `ssh`), never the suite user's.
   The `docker info` probe runs under the suite prefix below with `docker info` as its trailing
   command (so it carries `HOME` and `TMPDIR`), and must answer — the daemon socket probe, not
   merely `docker buildx version`.
4. **`slipfork-world-init` is resolved from Cargo, never guessed.** Both builds run under the
   suite prefix below (root has no rustup toolchain
   and does not own `target/`).
   `cargo build --locked -p slipfork-engine --bin slipfork-world-init --message-format=json`
   is parsed for the `compiler-artifact` whose `target.name` is `slipfork-world-init`; its
   `executable` field is the build-resolved path — the same artifact that
   `env!("CARGO_BIN_EXE_slipfork-world-init")` hands the suites. `test -x <path>` must pass and
   the path must equal the profile's `slipfork-world-init` attachment; either check failing
   exits non-zero naming the path and the Cargo invocation. The same run builds
   `-p slipfork --bin slipfork` in a second invocation so the `doctor` gate (step 5) and the
   AppArmor renderer (step 6) exist.
5. **Host-capability gate: one `doctor` run, never re-derived shell checks.**
   `/usr/sbin/runuser -u slipfork --
   /home/slipfork/slipfork/target/debug/slipfork doctor --json --state-dir
   /var/lib/slipfork`
   must exit `0`: the `ADR-0010` §4 report already covers the Btrfs super-magic, the
   `user_subvol_rm_allowed` scratch-subvolume round-trip, `unshare_userns`, and `netns`, so
   `test` never re-implements them with raw `statfs`/`id` probes. A non-zero exit fails closed
   printing the report and its `fixes` list verbatim; only then, as diagnostics, are
   `stat -f -c %t /var/lib/slipfork` and `id -u slipfork` echoed alongside it.
6. **AppArmor body equivalence.** The built CLI renderer must match the installed profile
   body; the rule, its fail-closed message, and the no-rewrite rule are §3.

   Steps 4–6 run in two identities by design: both `cargo build`/renderer invocations and the
   `doctor` gate use the
   `runuser -u slipfork` prefix below (the target directory belongs to `slipfork`); `test -x`,
   the comparison, and `aa-status` run as the root session user; and the control `unshare` is
   launched by root but must execute as `slipfork` (`runuser -u slipfork -- unshare …`),
   because a root-exempt control proves nothing about the unprivileged path. Neither identity
   is a suite runner: the suite commands below still start as `slipfork`.

The **suite prefix** is one fixed envelope shared by `fetch`, both builds, and both suites;
only the trailing command varies:

```console
/usr/sbin/runuser -u slipfork -- bash -c 'cd /home/slipfork/slipfork && exec env -i \
  HOME=/var/lib/slipfork/home TMPDIR=/var/lib/slipfork/home/tmp \
  CARGO_HOME=/home/slipfork/.cargo \
  RUSTUP_HOME=/home/slipfork/.rustup \
  PATH=/home/slipfork/.cargo/bin:/usr/local/bin:/usr/bin:/bin \
  <command>'
```

`fetch` (`/home/slipfork/.cargo/bin/cargo fetch --locked`) and the two `cargo build`
invocations of step 4 all run as that trailing command. It then streams these two suites, in
order and stopping at the first non-zero status:

```text
/home/slipfork/.cargo/bin/cargo test -p slipfork-engine \
  --test critical_btrfs_live -- --test-threads=1
/home/slipfork/.cargo/bin/cargo test -p slipfork \
  --test critical_btrfs_live -- --test-threads=1
```

`env -i` plus `HOME` on the Btrfs mount is deliberate: the suites derive every scratch and
cache root from `$HOME`, so all three real roots land on the volume — the engine suite's
`$HOME/.cache/slipfork/test-tmp` and `$HOME/.cache/slipfork/test-runtime`, and the CLI
suite's `$HOME/.cache/slipfork-cli-tests` (which also sets its own
`XDG_RUNTIME_DIR`/`XDG_DATA_HOME` under that scratch root). Both suites' `statfs(<state_dir>)`
probe therefore sees the provisioned volume instead of the droplet's ext4 root, and no
inherited `XDG_*` can redirect it. Without that, a suite would take its non-Btrfs
`BtrfsRequired` branch — the correct refusal assertion on a dev host (`M3.14` §3), but not the
kernel path — and the droplet run would prove nothing. Both suites are serialized because
they share one real Btrfs filesystem and real in-world ports; the engine suite fails first and
its failure stops the run, because a Btrfs/`crun` failure there invalidates the CLI suite's
premise. The command exits with the failing suite's status. The first run also fetches the
pinned static runtime asset and the `postgres:18`/`redis:8` image layers over the droplet's
network; those failures surface as the suite's own error, never as a harness pass. `crun` is
in-world only, never a host binary (§2 step 1).

### `ssh`

Resolves the single tagged droplet and `exec`s
`ssh -t -i <identity> root@IP 'cd /home/slipfork/slipfork &&
  export HOME=/var/lib/slipfork/root-home TMPDIR=/var/lib/slipfork/root-home/tmp &&
  export PATH=/home/slipfork/slipfork/target/debug:/home/slipfork/.cargo/bin:/usr/local/bin:/usr/bin:/bin &&
  command -v slipfork >/dev/null ||
    { echo "target/debug/slipfork missing; run tools/do_vm.py test first" >&2; exit 1; };
  exec bash'` with the fixed option set of §1, inheriting the terminal; a forwarded command
runs the same envelope with the forwarded command in place of `bash`. The owner `PATH` is the
suite `PATH` prefixed with `target/debug`, so bare `slipfork` resolves; the envelope asserts
`command -v slipfork` and fails closed naming `tools/do_vm.py test` (which builds
`target/debug/slipfork` in step 4) when the CLI is absent. No login shell intervenes, so no
profile can reset the exported `HOME`/`TMPDIR`/`PATH`. Every `slipfork` command
typed there therefore resolves `state_dir` to
`/var/lib/slipfork/root-home/.cache/slipfork` on the Btrfs volume
instead of `/root/.cache/slipfork` on ext4. `root-home` is root-owned `0700` and separate from
the suites' `HOME`, so owner commands never create root-owned directories inside the suite
user's home. `slipfork shell`/`prune` there operate on that root state root and require a
world the owner created in it (`slipfork up` first); unprivileged-path claims come only from
`test`.

With `-- <remote command> ...` (one leading `--` stripped, the remainder passed as one
`shlex`-quoted operand) it runs that command and exits with its status — the live-evidence path
for `findmnt`, `stat -f -c %t`, `btrfs subvolume list`, `slipfork shell -- crun --version`
(in-world, never a host binary), and `docker buildx version`.

### `down`

`--yes`-guarded, tag-scoped deletion: all tagged droplets first, then all tagged volumes,
then a bounded (60-second) poll for zero tagged resources, reporting any survivor instead of
claiming success. Non-tagged resources are never touched. Volumes are deliberately raw and
disposable; the Btrfs worlds on them have no backup path. Destroying a droplet removes its
`.slipfork/do-vm/known_hosts-<id>` file; the persistent identity key file is never deleted,
and a key imported into the DigitalOcean account stays there because the next droplet reuses
it.

---

## 2. Cloud-init provisioning (idempotent, bounded)

One `runcmd` chain (`bash -lc`, `set -euo pipefail`, `export DEBIAN_FRONTEND=noninteractive`),
also kept verbatim as the script `up` re-runs over SSH stdin when reconciling a late attach.
The script is produced by a pure `render_provision_script(...)` over the module constants
(volume glob, mountpoint, fstab path, profile path, marker path, remote tree), and
`cloud_init()` embeds its output unchanged, so `tools/tests` asserts the exact text — profile
body and marker path included — without a droplet. Readiness-marker ordering is normative in
step 7 (delete after mount verification) and step 11 (write last).

1. `apt-get update`, then `apt-get install -y --no-install-recommends btrfs-progs
   docker.io docker-buildx openssh-server apparmor curl ca-certificates build-essential git`
   (`apparmor` provides `apparmor_parser`, `curl`/`ca-certificates` serve rustup,
   `build-essential` links `zstd-sys`, and `git` is a real suite dependency — the CLI suite
   creates repositories and commits). No `crun` package: `crun` is not a host prerequisite —
   the SHA-256-pinned static `crun` ships inside the `L0` world rootfs (`ADR-0009` §1) and is
   executed only inside a world. Wait out a first-boot `dpkg` lock (bounded). On failure, print
   `apt-cache policy` for every package and stop. **No PPA, no third-party repo, no fallback.**
2. Create the unprivileged `slipfork` account when absent (`useradd --create-home --shell
   /bin/bash slipfork`) and add it to the `docker` group (`usermod -aG docker slipfork`), so
   the unprivileged suite can reach `docker buildx` for `L0` builds.
3. Wait (bounded) for exactly one `/dev/disk/by-id/scsi-0DO_Volume_*`; anything else fails
   closed listing what was found. `up` creates the volume through `doctl compute volume create`
   **without `--fs-type`**, so the device it creates is blank and carries none of the
   create-time metadata DigitalOcean's "Automatically Format & Mount" option writes; nothing
   auto-mounts it. A volume a human pre-formatted and attached may carry that automount,
   documented (`docs.digitalocean.com/products/volumes/how-to/{create,mount-unmount}`, fetched
   2026-11-16) as a `mnt-volume_*.mount` systemd unit plus
   `/etc/udev/rules.d/99-digitalocean-automount.rules` on systemd images (Ubuntu 24.04), with
   the fstab route applying only to non-systemd images. If one is present,
   `systemctl disable --now 'mnt-volume_*.mount'` (stop and disable the unit; `umount` it when
   no unit exists), then delete the udev rule and run `udevadm control --reload-rules`, delete
   the mount unit, and delete every fstab line whose
   mount point is under `/mnt/` and whose device is this volume, so nothing re-mounts it at
   boot. Re-verify the automount claim on the first live `up` (§9): it is external behaviour,
   not something this harness controls.
4. **Branch on `blkid`'s exit status; never coerce a failure into "fresh volume".** Run
   `blkid -p -s TYPE -o value "$dev"` (low-level probe: never a cached answer for a
   just-attached device), capturing stdout and stderr separately with an explicit
   `if out=$(...); then rc=0; else rc=$?; fi` idiom so `set -e` cannot abort before the
   branch, and switch on the status:
   - `0` with a non-empty value: `btrfs` is accepted; any other type fails closed naming the
     type and device, because `up` must not destroy data. `0` with empty output also fails
     closed (the status does not prove the value).
   - `2`: no signature, and the device must be proven **empty** before anything is written.
     All of the following are required, in this order, and any failure fails closed naming the
     device (and the exit status where one is captured):
     1. an empty `blkid` value;
     2. a successful bounded read, status-captured with the same
        `if dd …; then rc=0; else rc=$?; fi` idiom
        (`dd if="$dev" of=/dev/null bs=1M count=1`), so a read error names device and rc;
     3. the first MiB compares equal to `/dev/zero`
        (`cmp -s <(head -c 1048576 "$dev") <(head -c 1048576 /dev/zero)`); and
     4. `wipefs -n "$dev"` reports no signatures.

     Any non-zero byte, an unrecognized superblock, or a bare partition
     table fails closed naming the device, and `mkfs.btrfs -f -L slipfork "$dev"` runs only
     after all four checks pass.
   - **anything else** (`4` usage/other error, `8` ambivalent probe, mid-detach, any
     unexpected status) fails closed naming the device, the exit status, and the captured
     stderr.

   `blkid(8)` returns `2` both for "the specified token was not found" and for "impossible to
   gather any information", and a readable non-signature device (foreign superblock, bare
   partition table) also returns `2`, so rc `2` plus a successful read proves readability, not
   blankness — the emptiness proof above is what separates a fresh device from a merely
   unreadable or unrecognized one. A `|| true` would format a volume
   whose type could not be read, so it is forbidden. Capture the UUID with the same idiom
   (`blkid -p -s UUID -o value "$dev"`); a failed status or empty value fails closed.
5. `mkdir -p /var/lib/slipfork` (mountpoint only — nothing below it yet), delete every
   `/etc/fstab` line whose second field is `/var/lib/slipfork`, append
   `UUID=$uuid /var/lib/slipfork btrfs user_subvol_rm_allowed,noatime,nofail 0 0` (`nofail`
   keeps a detached or missing volume from blocking boot and locking the owner out); mount it
   when it is not mounted, and remount when `findmnt -no OPTIONS` lacks either required option
   (again captured with the explicit-status idiom, not `$(...)` bare under `set -e`). The
   fstab entry is what remounts the volume with the same options after a reboot.
6. Only **after** the mount: `install -d -o slipfork -g slipfork /var/lib/slipfork/home` and
   `install -d -o slipfork -g slipfork -m 700 /var/lib/slipfork/home/tmp` (the suites'
   `TMPDIR`, so cargo/rustc scratch is on disk, never `tmpfs`), then
   `install -d -o root -g root -m 700 /var/lib/slipfork/root-home` and
   `install -d -o root -g root -m 700 /var/lib/slipfork/root-home/tmp` (the owner `ssh`
   session's `HOME`/`TMPDIR`, §1), then `chown slipfork:slipfork /var/lib/slipfork`.
7. Verify, printing observed values on failure: `findmnt -no OPTIONS /var/lib/slipfork`
   contains `user_subvol_rm_allowed` and `noatime`, and `stat -f -c %t /var/lib/slipfork`
   prints `9123683e`. Then `rm -f /var/lib/slipfork/.slipfork-ready` — the marker is removed
   only now that the volume is mounted and verified, so it can never be the previous run's
   marker sitting on a freshly remounted volume.
8. Install Rust as `slipfork` from a **pinned** `rustup-init`, never the unpinned
   `curl https://sh.rustup.rs | sh` pipeline: download the versioned archive binary
   (`https://static.rust-lang.org/rustup/archive/<version>/x86_64-unknown-linux-gnu/rustup-init`)
   and verify it against a SHA-256 module constant before executing it with `-y --profile
   minimal --default-toolchain 1.94.0 --no-modify-path`; a digest mismatch fails closed naming
   the URL and both digests, the same pin discipline as `crun` (`ADR-0009` §1). Then require
   `/home/slipfork/.cargo/bin/cargo --version` >= 1.94 via the absolute path (workspace
   `rust-version = "1.94"`, edition 2024). `do_vm.py test`
   never depends on a manual toolchain step.
9. `systemctl enable --now docker`; require `docker buildx version` and
   `apparmor_parser --version` to answer. `crun` is not probed here or in `doctor` (§2 step 1);
   the `M3.19` live suite runs `slipfork shell -- crun --version` in-world.
10. Write the harness-owned `/etc/apparmor.d/slipfork` (§3) **only when it is absent or its
    body differs from the rendered text**, so a re-run never clobbers a matching profile;
    syntax-check it with `apparmor_parser -Q /etc/apparmor.d/slipfork`, load it with
    `apparmor_parser -r`, and prove effectiveness: `aa-status | grep -q slipfork` (the profile
    is loaded), and `runuser -u slipfork -- unshare --user --map-user=$(id -u slipfork)
    --map-group=$(id -g slipfork) -- true` succeeds. A rejection or a failed control is printed in full and fails
    closed. The sysctl stays at its Ubuntu default — the harness never disables the
    restriction.
11. Write `/var/lib/slipfork/.slipfork-ready` last, so an interrupted run can never look ready
    (ordering: step 7).

---

## 3. AppArmor `userns` profile

Ubuntu 24.04 LTS ships `kernel.apparmor_restrict_unprivileged_userns=1`, which denies
unprivileged `CLONE_NEWUSER` unless the caller's profile grants `userns,` (`ADR-0010` §7).

**One renderer, one writer.** The normative four-line profile is rendered by `slipfork daemon
unit --only apparmor` (`ADR-0010` §7) — **`M3.15` target state, not shipped when this design
was written**. The harness carries an explicitly interim copy of that authoritative body; it
introduces no second normative author: `render_provision_script`
emits the production body verbatim, and its only
permitted difference from the CLI rendering is the attachment list. Until `M3.15` lands, that
embedded text is the interim copy of the `ADR-0010` §7 body; the live preflight
below is what retires it.

**Who writes `/etc/apparmor.d/slipfork` on this droplet.** The harness's provisioning script
is the only writer, and it writes only when the file is absent or its body differs from the
embedded text, so re-running `up` never clobbers a profile whose body already matches.
Cloud-init necessarily writes the embedded text because the tree is synced only after `up`.
`slipfork` here is the binary built in the synced tree (`target/debug/slipfork`), never an
installed unit, so no other process on the VM renders that path and nothing redirects the CLI
renderer's stdout into the file.

**Body equivalence preflight (`test`).** After the synced tree is built, `test` runs
`target/debug/slipfork daemon unit --only apparmor` and compares the header and rule body with
the installed `/etc/apparmor.d/slipfork`, ignoring only the attachment list.
A CLI that rejects `--only apparmor` (pre-`M3.15` tree), a non-zero exit, or a divergent body
fails closed naming both texts. `test` never rewrites the profile: a divergence means the
harness's interim body and the CLI renderer have drifted, so the fix is to correct both in one
commit, delete the readiness marker
(`ssh -- rm -f /var/lib/slipfork/.slipfork-ready`), and re-run `up` — `up` re-runs the
provisioning script only when that marker is absent, so on a ready droplet it is otherwise a
no-op; because a reachable droplet satisfies the readiness probe at the first attempt, the
repair costs at most the 5-minute SSH gate (§1 `up` step 6), not the 15-minute bound.
The same byte comparison runs in CI with no droplet (`tools/tests` builds the CLI and compares
`render_provision_script()`'s embedded body with its `daemon unit --only apparmor` output), so
drift fails a unit test rather than only the live preflight.
The attachment path is checked separately (next bullets) and
must equal the profile's attachment, not be patched at suite time.

```text
abi <abi/4.0>,
profile slipfork /usr/bin/unshare /home/slipfork/slipfork/target/debug/slipfork-world-init flags=(unconfined) {
  userns,
}
```

- `/usr/bin/unshare` performs the `CLONE_NEWUSER` transition `slipfork-world-init` runs
  under (`unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc
  --pid --fork --mount-proc`). The suites start from a content-hashed `cargo test` executable
  under `target/debug/deps/` whose path changes per build, so the permission is attached to
  the executable that actually makes the syscall; without it `uid_map`/`mount`/`pivot_root`
  return `EPERM`.
- `slipfork-world-init` is a predicted default, not a hardcoded literal: the profile written at
  `up` spells the deterministic `/home/slipfork/slipfork/target/debug/slipfork-world-init`
  (the suites set no `CARGO_TARGET_DIR`; `sync` preserves `target/`), and `test` proves the
  prediction by requiring `test -x` on, and equality with, the
  `compiler-artifact.executable` Cargo reports for
  `-p slipfork-engine --bin slipfork-world-init` in the synced tree — the same path the engine
  suite gets from `env!("CARGO_BIN_EXE_slipfork-world-init")` and the CLI suite resolves next
  to `env!("CARGO_BIN_EXE_slipforkd")` (building it if absent). A mismatch fails closed naming
  both paths. Because the tree arrives after `up`, this attachment
  is only provable at the first live suite (`M3.19`), never during provisioning.
- `abi <abi/4.0>` and `flags=(unconfined)` are mandatory and the body is `userns,` only, per
  `ADR-0010` §7; no file or mount mediation is introduced.

---

## 4. Remote layout and the unprivileged run

| Path | Owner | Purpose |
|---|---|---|
| `/home/slipfork/slipfork` | `slipfork` | synced working tree (`target/` preserved across `sync`) |
| `/home/slipfork/{.cargo,.rustup}` | `slipfork` | toolchain (ext4, not state) |
| `/var/lib/slipfork` | `slipfork` | the Btrfs volume: worlds, artifacts, suites' state |
| `/var/lib/slipfork/home` | `slipfork` | the single `HOME` for every remote suite command *and* the `docker info` preflight |
| `/var/lib/slipfork/root-home` | `root` | `HOME`/`TMPDIR` for owner `ssh` commands (§1), so their `slipfork` state is on Btrfs and never collides with the suite user's home |

Transport and provisioning are `root` (apt, `mkfs.btrfs`, fstab, AppArmor); the suites
themselves always run as `slipfork` via `runuser`. That is not cosmetic: as root,
`BtrfsEngine` would record `$UID:$GID = 0:0`, `user_subvol_rm_allowed` and the AppArmor
restriction would be root-exempt, and every unprivileged-world claim in `ADR-0010` §4 and
`ADR-0009` §1 would pass vacuously. `test` fails closed if `slipfork` resolves to uid 0.
`docker` group membership is the only root-equivalent capability the suite user holds, and no
world-runtime path uses it. `ssh` stays an interactive `root` session for the owner; it is
never the suite runner.

---

## 5. SSH keys and the DigitalOcean registry quirk

Observed on the account backing this harness: a key can be registered on the DigitalOcean
account yet be invisible to the current `doctl` context, so `doctl compute ssh-key import`
was observed to fail with `422 ... SSH Key is already in use on your account` while
`doctl compute ssh-key list` showed a different key. Only the **bootstrap** (default)
identity touches the registry: match its public-key body (field 2 of `<path>.pub` against the
`public_key` field of `doctl compute ssh-key list --output json`), never a fingerprint
encoding, and import it when absent with `doctl compute ssh-key import slipfork-<sha256[:16]>
--public-key-file <path>.pub` (the digest is SHA-256 of field 2 of `<path>.pub`; key material
never enters argv). A `422` triggers exactly one
re-list; if the body is still invisible, `up` fails closed naming the path and the visible key
names **before creating anything** — it never strands a billed, unreachable droplet.

Additional `--identity` keys never touch the registry: their `.pub` is appended to
`/root/.ssh/authorized_keys` after boot (`sort -u`), so adding one and re-running `up`
converges and repeats leave the file byte-identical. Materialization and the
fail-closed-before-create checks for listed keys are §1 `up` step 2; an existing private key
is never regenerated.

SSH authentication rejections (`Permission denied (publickey)`, `Too many authentication
failures`) and host-key changes fail fast during readiness and `test` probes with identity
guidance, instead of burning the cloud-init timeout or reporting "cannot reach".

---

## 6. What the droplet actually proves

The block volume is one Btrfs filesystem and each world is one unified subvolume whose RO
snapshots and writable clones are metadata-only subvolume operations (`ADR-0010` §§2-4).
`user_subvol_rm_allowed` lets the unprivileged owner destroy its own snapshots and `noatime`
avoids read-amplified metadata writes that would distort clone measurements. The live engine
suite asserts a non-empty `btrfs subvolume list` while a world exists and an empty list after
clear-RO-then-destroy teardown, excluding the digest-keyed sealed base under
`<state_dir>/snapshots/` that legitimately survives until the Phase-2 `M3.2.13` GC
(`btrfs subvolume list /var/lib/slipfork | grep -v '/snapshots/'` empty); `btrfs-progs`
supplies that evidence.

Worlds spawn the pinned static `crun` (`rootfs::pin`, SHA-256 verified, cached under the
suites' `$HOME/.cache/slipfork/test-runtime`, staged into the world at
`/.slipfork/bin/crun`). `crun` is in-world only (§2 step 1); the evidence is
`slipfork shell -- crun --version` at the first live suite (`M3.19`).
`docker.io`/`docker-buildx` exist solely as `DockerfileStageBuilder`'s `BuildxExporter` builder
for the read-only `L0` rootfs (`ADR-0009` §1) — `dockerd` is never a world runtime.

The droplet supplies the exact Ubuntu 24.04 target: Linux 6.8+, AppArmor 4.0 with the
restriction enabled, and a real Btrfs block device. A world is unprivileged
`userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + `crun` — a namespace sandbox, not a
virtual machine. DigitalOcean droplets expose **no nested KVM** (`/dev/kvm` is absent), so
this harness verifies the namespace path only and nothing here or in published docs claims
VM isolation.

---

## 7. Cost model

The droplet price is read from `doctl compute size list` (`price_monthly`); block storage is
priced from the DigitalOcean list price as of 2026-11-16, `$0.10/GiB-month`, which is a
documented **estimate**, not a fetched quote (re-verify before `up`).
For the defaults in `ams3` at that price:

| Resource | Monthly |
|---|---|
| Droplet `s-4vcpu-8gb` (4 vCPU, 8 GiB, 160 GiB SSD) | `$48.00` (whatever doctl reports) |
| Volume `100 GiB` | `$10.00` (estimate) |
| **Total while `up`** | **`$58.00/month` (estimate)** |

`up` prints the line labelled as an estimate before asking for confirmation, and fails closed
only on the droplet price — if doctl reports no price, `up` stops; the volume term is never
presented as a quote. The volume exists (and bills) until `down --yes`; `up` itself never removes
it, and a droplet that is rebooting, unreachable, or idle bills exactly the same — which is
why the identity in §1 must outlive the session that created it.

---

## 8. Explicit non-goals

- **Not a supported deployment path.** No backups, no HA, no firewall hardening, no production
  secrets, no public documentation, no release artifact.
- The droplet is the only place real-kernel claims are measured; numbers stay there and are
  cross-checked in `docs/research/`, never generalized from a non-Btrfs host. Benchmark
  tooling (`uv`, browser binaries) is outside this harness: `M3.19` installs what
  `poc/run_benchmark.py` declares.
- No nested-KVM coverage.
- No automatic teardown: cost control is an explicit human `down --yes`.

---

## 9. Verification contract

`tools/tests/test_do_vm.py` drives the tool in-process (`main(argv) -> int`, no `sys.exit` in
handlers) with a fake `doctl`, fake `ssh`, patched clock/timeouts, and `HOME=tmp_path`,
asserting argv and stdout/stderr — never a real droplet. The fake `ssh` also consumes stdin,
so `sync`'s streamed tar is extracted into `tmp_path` and the resulting remote tree is
asserted. Two pure renderers are executed for real under `bash` with fake executables on a
`PATH` prefix: `render_volume_signature_step()` (fake `blkid`/`dd`/`head`/`cmp`/`wipefs`/
`mkfs.btrfs`) and
`render_automount_teardown()` (fake `findmnt`/`systemctl`/`umount`/`udevadm`), so the `blkid`
exit-status branch and the automount teardown are exercised, not string-matched;
`render_provision_script()` embeds both snippets verbatim. Every "Live evidence" cell is
proved on a real droplet by `M3.19`; `M3.16`'s own acceptance is unit-only.

| Claim | Unit test (no droplet) | Live evidence |
|---|---|---|
| `up` creates volume then droplet, never deletes | fake doctl call order, zero doctl `delete` calls, zero `mkfs` | `status` shows both |
| default identity generated `0600` in a `0700` dir, embedded | fake `ssh-keygen` writes a real pair; tool `chmod 0600`; doctl argv carries the body | `ssh -- true` after reboot |
| default identity path ignores `$XDG_CACHE_HOME`/`$XDG_DATA_HOME` | patched env -> `$HOME/.cache/slipfork/do-vm/id_ed25519`; unknown flag exits 2 | `up` summary prints the path |
| unknown/unimportable bootstrap key fails before any create | 422 + one re-list; still invisible -> exit 2 naming path and visible key names, zero creates | - |
| unreadable key or underivable `.pub` fails before create | unreadable private key and failing `ssh-keygen -y` -> exit 2, zero creates | - |
| extra `--identity` never touches the registry | zero `ssh-key import` calls | appended fingerprint |
| `--identity` rejects more than one path for `status`/`test`/`ssh`; rejected entirely by `sync`/`down` | two paths -> exit 2 usage error naming the flag; `sync`/`down` reject the flag with exit 2 | - |
| per-worktree `known_hosts` is named by droplet id | absolute `tmp_path/repo/.slipfork/do-vm/known_hosts-<id>` in ssh argv; `down` removes that file | host-key pin survives `up` rerun |
| non-destructive second `up` | zero create/attach/provision calls | second `up` is a no-op |
| duplicate/ambiguous tags or another-region resource | fake doctl 2 droplets / foreign-region droplet -> exit 2 naming both region slugs, zero creates | - |
| non-interactive `up` without `--yes` fails closed | `sys.stdin` is not a tty -> exit 2 | - |
| foreign volume signature refuses to format | bash-run signature snippet, fake `blkid` rc 0 = `ext4` -> exit 2 naming type+device, zero `mkfs`; rc 0 with empty output -> exit 2 | - |
| `blkid` error fails closed, never formats | fake `blkid` rc 4 and rc 8 with a stderr needle -> exit 2 naming device, rc, stderr, zero `mkfs` | - |
| blank device (`blkid` rc 2) formats exactly once, rerun keeps it | fake `blkid` rc 2 + all-zero first MiB + `wipefs -n` clean -> exactly one `mkfs`; a failed read (rc named), a non-zero first MiB (unrecognized superblock or bare partition table), and a `wipefs -n` signature with an otherwise all-zero first MiB each -> exit 2, zero `mkfs`; rerun rc 0 `btrfs` -> zero | `findmnt` options |
| cloud-init timeout prints logs and deletes nothing | fake ssh exit 1 + patched sleep | - |
| missing suite / missing remote copy fail closed | stub suite files in the tmp repo; remote probe exit 1 -> exit 2 naming both paths | - |
| `test` preflights doctor, cargo, docker | one `doctor --json --state-dir /var/lib/slipfork` argv under the suite prefix (fake ssh prints XFS magic `58465342`, then Btrfs `9123683e`, uid 0, no cargo); raw `stat`/`id` echoed only when `doctor` exits non-zero; `docker info` argv carries `HOME=/var/lib/slipfork/home` | streamed `test` output |
| one `HOME` on the volume for every remote command | fetch, build, both suites, and the `docker info` probe (all under the suite prefix) carry `HOME=/var/lib/slipfork/home` and `TMPDIR=/var/lib/slipfork/home/tmp` | suites' roots under `/var/lib/slipfork/home/.cache` |
| owner `ssh` commands resolve state on the volume | interactive wrapper argv carries `cd /home/slipfork/slipfork`, owner `PATH` with `target/debug` first, `HOME`/`TMPDIR=/var/lib/slipfork/root-home[/tmp]`, and the `command -v slipfork` assertion; forwarded command argv carries the same envelope with that command | `ssh -- pwd` and `ssh -- slipfork doctor` |
| `slipfork-world-init` path comes from Cargo, `test -x` enforced | fake artifact JSON -> profile attachment; missing executable or path mismatch -> exit 2; build argv carries the `runuser` + `env -i` prefix | `grep '^profile' /etc/apparmor.d/slipfork` shows the attachment |
| AppArmor body equals the CLI renderer, attachments excepted | `compare_profile_body()` accepts an attachment-only difference and rejects a rule/flag difference; renderer-rejected -> exit 2 naming both texts | live `test` preflight against the built CLI |
| embedded profile body equals the CLI renderer offline (no droplet) | CI builds `target/debug/slipfork` and compares `render_provision_script()`'s body with `daemon unit --only apparmor`; a mismatch fails naming both texts | - |
| `cargo fetch` failure is a toolchain/network error | fake ssh fetch exit 1 -> zero suite invocations | - |
| host-key change and publickey denial name the fix | fake ssh stderr variants | - |
| suite order, serialization, fail-fast | fake ssh exit 101 -> CLI suite never invoked; tool exits 101 | streamed `test` output |
| droplet run takes the Btrfs branch, not `BtrfsRequired` | - | non-empty `btrfs subvolume list` mid-run; `grep -v '/snapshots/'` empty after teardown |
| readiness marker is inside the mount and deleted after mount | fake ssh `test -f /var/lib/slipfork/.slipfork-ready`; rendered script deletes it only after the `9123683e` check | reboot re-check (`M3.19`) |
| provisioning script embeds verbatim in cloud-init | `render_provision_script() in cloud_init()` and both snippets embedded verbatim; pinned `rustup-init` URL and SHA-256 constant present | cloud-init log |
| no host `crun` is installed or probed; `git` is | rendered `apt-get install` package list contains `git` and no `crun`; zero `crun` probes in any argv | `slipfork shell -- crun --version` (`M3.19`) |
| DigitalOcean automount teardown is conditional and complete | bash-run teardown snippet, fake `findmnt`/`systemctl`/`umount`/`udevadm`: blank volume -> zero removals; pre-formatted -> `systemctl disable --now` the unit, then udev rule + `udevadm control --reload-rules`, mount unit, and fstab line | `systemctl list-units --all 'mnt-volume_*.mount'` empty, `test ! -e /etc/udev/rules.d/99-digitalocean-automount.rules`, `findmnt /mnt/volume_*` empty |
| cost from doctl, absent price fails closed | fake size list `$1.00` -> `$11.00/month` printed as an estimate; `null` -> exit 2 | `up` summary line |
| secrets and key bodies never logged | needles: private-key body and `.env` sentinel; haystacks: argv, stdout, stderr, rendered cloud-init | - |
| `sync` replaces the tree and prunes | fake ssh extracts the stdin tar into `tmp_path`; deleted file absent, `target/` preserved | `test` after delete |
| `down` polls to zero and keeps the identity | fake doctl survivor -> non-zero; zero ssh calls; key file intact; `known_hosts-<id>` removed | `down` re-list |
