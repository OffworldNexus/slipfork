# M3.16 — `tools/do_vm.py` Btrfs dev-VM harness (attempt 1)

## Status

`done` — live proof complete on droplet `620073485` (`165.232.86.174`, ams3), volume
`131a2765-e1ba-11f1-b67b-d6114774f516` now Btrfs `LABEL="slipfork"`. The droplet is **left
running** for the owner's 0.1.0 hands-on test and `M3.19`.

```console
ssh -i ~/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174
```

## Quad Review Gate fixes (one pass)

| Fix | Finding | Change |
|---|---|---|
| A | **Safety BLOCKER**: the rc 2 "blank" proof inspected only the first MiB + `wipefs -n`; this session proved a label survives at offset 1 MiB while `blkid` rc 2, `cmp` of MiB 0, and `wipefs -n` were all clean. | `up` formats only when **this invocation created the volume** or the operator passed the new `--format-blank`. The proof is now `blkid` rc 2 **and** clean `wipefs -n` **and** the first **8 MiB** read back all zero (label copies live at 0..4 MiB; the reason is in a source comment). A detected foreign signature (`blkid` rc 0, non-`btrfs`) always fails closed and `--format-blank` never overrides it. Default `up` on a pre-existing blank volume fails with `slipfork: blank volume detected; re-run with --format-blank after confirming it holds no data`. The exact evidence line is printed on success. Unit tests cover (i) blank without the flag, (ii) flag + 8 MiB zeros → one `mkfs`, (iii) foreign `ext4` with the flag → fail, (iv) a non-zero byte in MiB 1..8 → fail. |
| B | MAJOR: `--identity` suppressed default materialization; `status`/`test`/`ssh` rejected a second path. | `up` always materializes the default `~/.cache/slipfork/do-vm/id_ed25519` and treats listed `--identity` paths as additional authorized keys, so `up --identity X && sync` works. The default path is always `up`'s transport identity; `status`/`test`/`ssh` use the first listed path. `--identity` is repeatable on all four commands (no arity error). Test: two paths accepted, first used in `status` SSH argv. |
| C | MAJOR: speculative `test -- <cargo args>`. | Removed the `REMAINDER`, the `extras` append, and the `--nocapture` test; `test` streams exactly the two suites with `-- --test-threads=1`. Docstring updated. |
| D | MAJOR: four hand-rolled deadline loops. | One `_wait_until(probe, timeout_seconds, on_timeout)` now backs volume detach, droplet deletion, account-key propagation, and the `down` poll; every deadline/interval and fail-closed diagnostic is preserved. |
| E | minors | `_materialize_identity` tightens an existing parent to `0700`; `ensure_account_key` runs after the cost `_confirm` (a declined `up` no longer mutates the account); `--authorized-key` files are read/validated before the confirm/create block; `systemctl list-units` failure fails closed with captured stderr; `cmd_test` reuses `_ready_probe`; one canonical root+slipfork `authorized_keys` enumeration (`authorized_keys_paths`, `render_install_authorized_keys`); `REMOTE_HOME` added; identity help derives from `default_identity_path()`; `SUITES` derives from `SUITE_NAME`; tag messages use `TAG`; `_transport_failure` is the single auth/transport error; `owner_envelope` / `owner_shell_envelope` split; one `Doctl` plus `Path.cwd()`/`Path.home()` resolved in `main()` and threaded through `args`; tests use one `run_bash` helper, a neutral `owner@example` fixture key, and `default_identity_path()`; the crun/zfs/incus absence asserts live only in the dedicated test. |

### Re-gate round 2 fixes

| Fix | Finding | Change |
|---|---|---|
| 1 | MAJOR: `_provision` called `_transport_failure` for every non-zero exit, so a genuine provisioning refusal was misreported as "cannot reach" and the real error was dead code. | `_provision` now calls `_transport_failure` only when the captured detail matches a host-key or auth failure; otherwise it raises `ProvisioningError("provisioning on <ip> failed (<exit>): <detail>")`, and `main` returns that remote exit status. Test: fake SSH exits 1 with the blank-volume stderr → `up` exits 1, stderr names `blank volume detected`, and `cannot reach` is absent. |
| 2 | MINOR: `up --identity` help claimed the first listed path is the transport identity, but `up` always uses the default bootstrap key. | `up` has its own help: listed identities are additional public keys installed for root/slipfork, and the default `<path>` is always the transport identity. `status`/`test`/`ssh` keep the "first listed path is used" text. |
| 3 | MINOR: coverage gaps. | Added: pre-existing volume without `--format-blank` renders `_format_allowed=0` in cloud-init/user-data, with the flag renders `_format_allowed=1`; the format case asserts exactly one `mkfs` call; a malformed-`doctl` test (`networks.v4` not a list) exits 2 with zero creates. |
| 4 | MINOR: docstring over-claimed "the only destructive command is `down --yes`"; the 8 MiB read hid its stderr. | Docstring now says `down --yes` is the only command that deletes the volume. The 8 MiB read captures stderr into a temp file and reports it on failure (fake `dd` test asserts `Input/output error` is surfaced). |

### Re-gate round 3 fixes

| Fix | Finding | Change |
|---|---|---|
| 1 | MAJOR: `AUTHORIZED_KEY_USERS` was dead while the install renderer re-hardcoded `root <user>`. | `render_install_authorized_keys` now derives the user list from the single `AUTHORIZED_KEY_USERS` constant; no second hardcoded pair. |
| 2 | MAJOR: `ProvisionConfig.apparmor_body` was never set by any caller/test. | Deleted the field and the `is not None else` fallback; `render_provision_script` calls `render_apparmor_body(config.remote_tree)` unconditionally (the renderer stays the pre-binary single source; `M3.15` owns byte-equivalence). |
| 3 | MINOR: `_poll` was a hand-rolled deadline loop beside `_wait_until`, and `_wait_ssh`/`_wait_ready` were twins. | One `_poll_or_raise(probe, …, message)` now sits on `_wait_until`; `_wait_ssh` and `_wait_ready` keep their exact deadlines and fail-closed messages, with the ready path's log dump in its `on_timeout` callback. |
| 4 | MINOR: unused `Harness.read_state`; repeated droplet+volume state setup. | Deleted `read_state`; added `Harness.existing()` and replaced the eight mechanical droplet+volume pairs. Tests with custom state are untouched. |
| 5 | MINOR: `--format-blank` help understated the risk. | Help now ends `DESTROYS any data on the volume; never overrides a detected foreign signature`. |
| 6 | MINOR: RESULT row B still implied `up` uses the first listed identity. | Corrected: the default path is always `up`'s transport identity; listed identities are additional authorized keys; `status`/`test`/`ssh` use the first listed path. |
| 7 | MINOR: `tools/tests/test_tools.py` trips the repo-wide terminology grep. | Not edited (out of scope); recorded as a cross-node followup for `M3.23`. |
| IoC-1 | `_provision` could report SSH 255 as a provisioning error. | Exit 255 is now a transport failure unconditionally (like `cmd_test`); test: 255 + `Connection refused` → `up` exits 2 with `cannot reach … over SSH`, no `provisioning on`. |
| IoC-2 | `_add_identity` read `Path.home()` inside `build_parser`. | `main` resolves `home` once and passes it to `build_parser(home)`; `_add_identity` derives the default from that value. No ambient home read outside `main`. |
| IoC-3 | `_stream_tar` called `subprocess.Popen` directly. | Added `_run_stream(argv)` beside `_run` as the single streaming-process seam; `_stream_tar` routes through it with unchanged stdin/return-code semantics. |

### Corrected safety claim (honest record)

The pre-gate RESULT said the harness "still refuses any foreign signature". That was too strong:
the old rc 2 branch could format a device whose foreign label lived **past the first MiB** while
`blkid` returned rc 2, the first MiB compared equal to zero, and `wipefs -n` was empty. This
session produced exactly that counterexample on the live volume: after a 1 MiB `dd`,
`blkid /dev/sda` was rc 2, `cmp` of MiB 0 was 0, `wipefs -n` was empty, yet
`blkid -p -O 1048576 -s TYPE -o value /dev/sda` reported `zfs_member` (`incuspool`). The old
proof would have formatted a foreign filesystem. The new gate (created-volume or
`--format-blank`, plus the 8 MiB zero proof) and the always-fail-closed detected-signature branch
close that hole. The one-time H-005 wipe itself was an operator action over SSH; `up` gained no
wipe flag and no fallback.

## What changed (cumulative)

`tools/do_vm.py` rewritten for the Btrfs substrate; `tools/tests/test_do_vm.py` drives it
in-process with fake `doctl`/`ssh` executables.

- Six commands (`up`, `status`, `sync`, `test`, `ssh`, `down`), tag-scoped find-or-create,
  `--yes` guards, bounded waits, fail-closed `doctl` parsing, stdlib only, `# /// script`,
  no `shell=True`, no account literals.
- Persistent `--identity` (default `~/.cache/slipfork/do-vm/id_ed25519`): generated `0600` in a
  `0700` dir, passphrase rejected via `ssh-keygen -y -P ''`, `.pub` derived/verified, imported
  into the DO account when `doctl` cannot see it (bounded registry-propagation wait after a
  successful import; one re-list then fail closed on a failed import). Repeatable everywhere it
  is accepted; the first listed path is used by `status`/`test`/`ssh`.
- `--authorized-key <PATH>` repeatable; `up` installs the union of the bootstrap identity,
  every extra `--identity`, and every `--authorized-key` into `authorized_keys` for **root and
  `slipfork`** (`sort -u`, idempotent).
- Recreate-on-same-volume: when the tagged droplet exists but the identity is rejected, `up`
  detaches the volume (bounded wait for `droplet_ids` to clear), deletes the droplet, waits for
  zero tagged droplets, and recreates it with `--volumes <same-id>`. Host-key changes fail
  closed naming the per-worktree `known_hosts`; they never trigger a recreate.
- Provisioning (one idempotent script, embedded verbatim in cloud-init and re-run over SSH):
  `btrfs-progs docker.io docker-buildx openssh-server apparmor curl ca-certificates
  build-essential git`, `slipfork` user + `docker` group, DigitalOcean automount teardown
  (fail-closed `systemctl`), volume-disk discovery that ignores `-partN` symlinks, the
  format gate + 8 MiB `blkid`/`wipefs` proof above, fstab
  `UUID=… /var/lib/slipfork btrfs user_subvol_rm_allowed,noatime,nofail`, mount/remount +
  `findmnt` option check + `stat -f -c %t` == `9123683e`, suite/owner homes, pinned
  `rustup-init` 1.29.3 (SHA-256 `b1c8cb14…6c55`) + Rust 1.94.0, `docker buildx version`,
  `apparmor_parser --version`, the four-line `/etc/apparmor.d/slipfork` `userns` profile
  (permitting `/usr/bin/unshare` and `slipfork-world-init`) with `-Q`/`-r`/`aa-status`/
  `runuser … unshare --user` control, and the readiness marker written last inside the mount.
  No host `crun`; no ZFS/Incus package, path, flag, or marker.
- `sync`: `.env` refusal unchanged, `tar`-over-SSH into `/home/slipfork/.sync`, then replace
  `/home/slipfork/slipfork` preserving `target/`, `chown -R slipfork:slipfork`.
- `test`: fails closed naming both suite paths and `M3.14`/`M3.15` while absent; then checks the
  readiness marker, both synced suites, `cargo --version` >= 1.94.0, `docker info` under the
  suite envelope; streams `cargo test -p slipfork-engine --test critical_btrfs_live` then
  `cargo test -p slipfork --test critical_btrfs_live`, both `-- --test-threads=1`, stopping at
  the first non-zero status and returning it. No extra cargo arguments.
- `ssh`: identity + fixed config-free option set, owner envelope (`HOME`/`TMPDIR` on the Btrfs
  volume, `CARGO_HOME`/`RUSTUP_HOME` for the toolchain, `target/debug` first on `PATH`); the
  interactive session asserts `command -v slipfork` and `exec bash --noprofile --norc`;
  `ssh -- <cmd>` forwards the command without that assertion so the live-evidence commands work
  on a freshly provisioned droplet.
- `down`: keeps the volume by default (`--delete-volume` drops it), droplet first, bounded poll
  to zero, survivors reported instead of claiming success, identity never deleted.

## Authorized wipe (one-time operator action, H-005)

The tagged volume held the retired ZFS pool, not Btrfs. After the owner's "Wipe ZFS & format
Btrfs (Recommended)" authorization, the wipe was performed by hand over the harness `ssh`
path. **`up`'s guard is unchanged in intent and stronger in proof** (see FIX A): its first run
on the ZFS volume failed closed exactly as designed, and the current rc 2 branch can no longer
format an undetected foreign label.

```console
$ tools/do_vm.py ssh -- lsblk -f
NAME    FSTYPE     FSVER LABEL           UUID                                 MOUNTPOINTS
sda
├─sda1  zfs_member 5000  incuspool       313917810000357958
└─sda9
vda
├─vda1  ext4       1.0   cloudimg-rootfs 59134bd0-a76f-4ea0-8e4a-770b2e1eafc2  /
...
```

`wipefs`/`blkid` are in `/usr/sbin`, which the owner envelope's suite-style `PATH` omits, so the
wipe used absolute paths (the provisioning script runs with the full root `PATH`):

```console
$ tools/do_vm.py ssh -- bash -lc 'if [ -e /dev/sda1 ]; then /usr/sbin/wipefs -af /dev/sda1; else echo NO_PARTITION; fi'
NO_PARTITION                      # the first dd below had already zeroed the primary GPT
$ tools/do_vm.py ssh -- /usr/sbin/wipefs -af /dev/sda
/dev/sda: 8 bytes were erased at offset 0x18fffffe00 (gpt): 45 46 49 20 50 41 52 54
$ tools/do_vm.py ssh -- dd if=/dev/zero of=/dev/sda bs=1M count=1 conv=fsync
1+0 records in
1+0 records out
1048576 bytes (1.0 MB, 1.0 MiB) copied, 0.0123806 s, 84.7 MB/s
$ tools/do_vm.py ssh -- bash -lc '/usr/sbin/blkid /dev/sda; echo rc=$?'
rc=2
$ tools/do_vm.py ssh -- bash -lc 'cmp -s <(head -c 1048576 /dev/sda) <(head -c 1048576 /dev/zero); echo cmp=$?'
cmp=0
$ tools/do_vm.py ssh -- /usr/sbin/wipefs -n /dev/sda      # no output
```

A residual ZFS label still lived just past 1 MiB (`blkid -p -O 1048576 -s TYPE -o value
/dev/sda` → `zfs_member`; the old partition began at 1049 kB), so the authorized "wipe ZFS" was
completed by zeroing the first 8 MiB (primary GPT + all four ZFS label copies):

```console
$ tools/do_vm.py ssh -- dd if=/dev/zero of=/dev/sda bs=1M count=8 conv=fsync
8+0 records in
8+0 records out
8388608 bytes (8.4 MB, 8.0 MiB) copied, 0.0371423 s, 226 MB/s
$ tools/do_vm.py ssh -- bash -lc '/usr/sbin/blkid /dev/sda; echo rc=$?'
rc=2
$ tools/do_vm.py ssh -- /usr/sbin/blkid -p -O 1048576 -s TYPE -o value /dev/sda   # rc=2, empty
$ tools/do_vm.py ssh -- bash -lc 'cmp -s <(head -c 8388608 /dev/sda) <(head -c 8388608 /dev/zero); echo cmp=$?'
cmp=0
$ tools/do_vm.py ssh -- /usr/sbin/wipefs -n /dev/sda      # no output
```

The guard was proven on the same volume before the wipe: `up` failed closed with

```text
error: provisioning 165.232.86.174 failed (1): slipfork: blkid reported success but no filesystem type for /dev/disk/by-id/scsi-0DO_Volume_slipfork-dev-vm; wipefs -n says:
DEVICE                          OFFSET       TYPE UUID LABEL
scsi-0DO_Volume_slipfork-dev-vm 0x200        gpt
scsi-0DO_Volume_slipfork-dev-vm 0x18fffffe00 gpt
scsi-0DO_Volume_slipfork-dev-vm 0x1fe        PMBR
```

No `mkfs` ran; the volume was untouched by the harness.

## Provisioning: live fixes and final `up`

The post-wipe `up` exposed four real defects, each fixed and unit-covered:

1. `rustup-init` derives its proxy name from `argv[0]`; a `mktemp` file named `tmp.XXXX` fails
   with `unknown proxy name: 'tmp'` → download to `<mktemp -d>/rustup-init`.
2. That `mktemp -d` directory is `0700`, so `slipfork` could not execute the binary
   (`Permission denied`) → `chmod 755` the short-lived directory.
3. The design-of-record AppArmor header (`profile slipfork <path> <path>`) is rejected by the
   live parser (`syntax error, unexpected TOK_ID, expecting TOK_OPEN`); the valid single
   attachment expression is `/{usr/bin/unshare,<tree>/target/debug/slipfork-world-init}`.
4. `ssh --` forwarded commands required the synced tree and built CLI, blocking the documented
   live-evidence commands on a fresh droplet → the `command -v slipfork` assertion now guards
   only the interactive shell; `CARGO_HOME`/`RUSTUP_HOME` are exported so `rustc`/`cargo` work.

Final `up` (exit 0):

```text
droplet  slipfork-dev-vm  id=620073485  region=ams3  ip=165.232.86.174
volume   slipfork-dev-vm  id=131a2765-e1ba-11f1-b67b-d6114774f516  region=ams3  100 GiB
btrfs    /var/lib/slipfork mounted with user_subvol_rm_allowed,noatime,nofail; marker /var/lib/slipfork/.slipfork-ready
identity /home/remy/.cache/slipfork/do-vm/id_ed25519
ssh      ssh -i /home/remy/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174
cost     $48.00/month droplet (s-4vcpu-8gb, ams3) + $10.00/month volume (100 GiB, estimate) = $58.00/month (estimate)
keys     authorized_keys fingerprints:
         == /root/.ssh/authorized_keys
         256 SHA256:nZVhV69SFiOZLtnUMnPnu982J+lqO/40YjMiQiqKSu8 slipfork-do-vm (ED25519)
         2048 SHA256:SGXbyi5u4i8RO0JgxubkI2RZ2N7M4bCFB4HdUkgas9w remy@doc (RSA)
         == /home/slipfork/.ssh/authorized_keys
         256 SHA256:nZVhV69SFiOZLtnUMnPnu982J+lqO/40YjMiQiqKSu8 slipfork-do-vm (ED25519)
         2048 SHA256:SGXbyi5u4i8RO0JgxubkI2RZ2N7M4bCFB4HdUkgas9w remy@doc (RSA)
```

## Live proof (verbatim, read-only re-check after the gate fixes)

```console
$ tools/do_vm.py status
droplet  slipfork-dev-vm  id=620073485  region=ams3  size=s-4vcpu-8gb  ip=165.232.86.174
volume   slipfork-dev-vm  id=131a2765-e1ba-11f1-b67b-d6114774f516  region=ams3  100 GiB
... (fingerprints as above) ...
ssh      ssh -i /home/remy/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174

$ tools/do_vm.py ssh -- findmnt -no SOURCE,OPTIONS /var/lib/slipfork
/dev/sda rw,noatime,discard=async,space_cache=v2,user_subvol_rm_allowed,subvolid=5,subvol=/

$ tools/do_vm.py ssh -- stat -f -c %t /var/lib/slipfork
9123683e

$ tools/do_vm.py ssh -- bash -lc 'test -f /var/lib/slipfork/.slipfork-ready && echo READY'
READY

$ tools/do_vm.py ssh -- grep -c remy@doc /root/.ssh/authorized_keys /home/slipfork/.ssh/authorized_keys
/root/.ssh/authorized_keys:1
/home/slipfork/.ssh/authorized_keys:1

$ tools/do_vm.py ssh -- grep -c slipfork-do-vm /root/.ssh/authorized_keys /home/slipfork/.ssh/authorized_keys
/root/.ssh/authorized_keys:1
/home/slipfork/.ssh/authorized_keys:1

$ tools/do_vm.py ssh -- rustc --version
rustc 1.94.0 (4a4ef493e 2026-03-02)
$ tools/do_vm.py ssh -- cargo --version
cargo 1.94.0 (85eff7c80 2026-01-15)
$ tools/do_vm.py ssh -- docker buildx version
github.com/docker/buildx 0.30.1 0.30.1-0ubuntu1~24.04.1
$ tools/do_vm.py ssh -- bash -lc '/usr/sbin/aa-status | grep -n slipfork'
107:   slipfork
$ tools/do_vm.py ssh -- /usr/sbin/apparmor_parser -Q /etc/apparmor.d/slipfork
$ tools/do_vm.py ssh -- cat /etc/apparmor.d/slipfork
abi <abi/4.0>,
profile slipfork /{usr/bin/unshare,/home/slipfork/slipfork/target/debug/slipfork-world-init} flags=(unconfined) {
  userns,
}

$ tools/do_vm.py ssh -- /usr/sbin/blkid /dev/sda
/dev/sda: LABEL="slipfork" UUID="c3b7822d-a068-4792-917c-5051a0a19d12" UUID_SUB="20878830-6903-41fc-acbd-cc1b9d34f441" BLOCK_SIZE="4096" TYPE="btrfs"
$ tools/do_vm.py ssh -- lsblk -f
sda     btrfs         slipfork        c3b7822d-a068-4792-917c-5051a0a19d12     98G     0% /var/lib/slipfork
$ tools/do_vm.py ssh -- /usr/bin/btrfs subvolume list /var/lib/slipfork   # empty (no worlds yet)

$ ssh -i ~/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174 true
# exit 0
```

The owner's private key file is not on this host (only `~/.cache/slipfork/remy-doc.pub`), so the
owner-key login itself was not run here; its presence for both users is proven by the
`grep -c remy@doc` counts and the `ssh-keygen -lf` fingerprints above.

The live droplet was provisioned before the Quad-Gate safety fix and was **not** re-run (owner
instruction: no `up`/`down`/delete on the live droplet). The live volume is already Btrfs, so a
future `up` takes the `blkid` rc 0 `btrfs` branch and never reaches the format gate; the new
gate itself is covered by the unit matrix below.

## Static acceptance (final)

| Command | Result |
|---|---|
| `uv run --no-project tools/do_vm.py --help` | rc 0, six commands |
| `uv run --no-project tools/do_vm.py up --help` | rc 0, `--identity`, `--authorized-key`, `--format-blank`, `--yes` |
| `! grep -rniE 'zfs\|zpool\|incus\|zfsutils' tools/do_vm.py` | no output, rc 1 (passes) |
| `grep -n 'mkfs.btrfs' tools/do_vm.py` | line 364 |
| `grep -n 'user_subvol_rm_allowed,noatime' tools/do_vm.py` | lines 9, 79, 457 |
| `grep -n 'identity' tools/do_vm.py` | 75 matches |
| `grep -n 'apparmor' tools/do_vm.py` | 9 matches |
| `grep -n 'format-blank' tools/do_vm.py` | docstring, the blank-volume refusal, and the `up` flag |
| `uv run --no-project --with pytest pytest -q tools/tests` | 112 passed |
| `uvx --offline ruff check tools && ruff format --check tools` | rc 0 |
| `! grep -rn 'dogeops\|DogeOps\|60390733' tools/do_vm.py` | no output, rc 1 (passes) |
| `uv run --no-project tools/guard.py` | `guard: OK (487 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | `plan OK` |
| `! grep -riE 'option [ab]\|spike [ab]' tools/do_vm.py tools/tests/test_do_vm.py` | no output, rc 1 (passes) |
| `uv run --no-project tools/do_vm.py status` (read-only) | rc 0, droplet `620073485` + volume `131a2765…` |

## Unit coverage for the new branches

- Format gate / `blkid` rc matrix (11 cases, executed under real `bash` with fake
  `blkid`/`dd`/`wipefs`/`mkfs.btrfs`): rc 0 `btrfs` → no `mkfs`; rc 0 `ext4` **with the flag** →
  fail naming type+device; rc 0 empty → fail; rc 2 blank **without the flag** → fail with the
  exact `--format-blank` message; rc 2 + 8 MiB zeros **with the flag** → exactly one `mkfs` and
  the evidence line; rc 2 with a non-zero byte in MiB 1..8 → fail; rc 2 `wipefs` signature →
  fail; rc 2 failed 8 MiB read → fail naming rc; rc 4/8 → fail naming device/rc/stderr; UUID
  failure → fail.
- Volume discovery: disk + `-part1` + `-part9` selects the disk; two disks fail closed listing
  both (executed under `bash`).
- Key import/recreate decision: invisible key imported then embedded; import failure + still
  invisible → exit 2 naming path and visible keys, zero creates; successful import that never
  becomes visible → exit 2, zero creates; auth rejection → detach → delete → recreate on the
  same volume; host-key change → fail closed, zero deletes.
- Authorized-keys union: bootstrap + extra `--identity` + `--authorized-key` bodies in both
  `root` and `slipfork` files; install script targets `for _user in root slipfork` and `sort -u`.
- Repeatable `--identity`: two paths accepted on `status`, the first is the SSH identity.
- `no shell=True`: static source assert plus a runtime spy on every `subprocess.run` kwargs
  during `up`.
- AppArmor body: four lines, leading-slash alternation attaching both executables.
- Automount teardown: blank volume → no removals; pre-formatted → unit/umount/udev/fstab
  cleanup; `systemctl list-units` failure → fail closed with captured stderr.
- Provisioning refusal: remote exit 1 with `blank volume detected` → `up` exits 1 with
  `provisioning on <ip> failed (1): …`, never "cannot reach".
- Format decision reaches cloud-init: `_format_allowed=0` for a pre-existing volume without the
  flag, `_format_allowed=1` with it; the success case runs exactly one `mkfs`.
- Malformed `doctl` JSON (`networks.v4` not a list) fails closed with zero creates; the 8 MiB
  read's stderr is surfaced on failure.
- Plus: six-command help, identity `0600`/`0700` and `$XDG_CACHE_HOME` independence,
  non-destructive second `up`, non-interactive `--yes` guard (and no account mutation),
  duplicate tags/foreign region, price fail-closed, cloud-init timeout logs, status fingerprints,
  `sync` `.env` refusal and staging replace, `down` keep/delete/survivors, `ssh`
  envelope/forwarding, secrets and private key never logged, provision script `bash -n`,
  cloud-init embedding and `_format_allowed` propagation.

## Notes / followups

- The wipe was a **one-time owner-authorized operator action** (H-005). `tools/do_vm.py` gained
  no wipe flag and no fallback; formatting a pre-existing blank volume now requires the explicit
  `--format-blank` plus the 8 MiB zero proof, and a detected foreign filesystem always fails
  closed.
- The droplet is left **up** (`620073485`, `165.232.86.174`) for the owner's 0.1.0 hands-on test
  and `M3.19`; `down --yes` keeps the volume, `down --yes --delete-volume` drops it.
- **Cross-node followups (out of this node's paths):** `tools/tests/test_tools.py`'s "Option A"
  fixture trips the repo-wide terminology grep; `M3.23` owns that sweep. The design of record
  `docs/design/do-vm-harness.md` still says two `--identity` paths are a usage error; the tool
  now accepts a repeatable flag (first listed path wins for `status`/`test`/`ssh`), and `M3.23`
  owns the doc correction.
- **Cross-node ruling:** the AppArmor byte-equivalence test belongs to `M3.15`
  (`daemon unit --only apparmor`); this node keeps the harness renderer as the pre-binary single
  source and does not add the byte comparison.
- Design-of-record corrections to port back to `docs/design/do-vm-harness.md` (outside this
  node's paths): the AppArmor header must use `/{a,b}` (two bare attachment paths are invalid
  syntax); the owner envelope must not require the built CLI for forwarded diagnostic commands
  and must export `CARGO_HOME`/`RUSTUP_HOME`; volume discovery must ignore `-partN` symlinks;
  the blank-volume proof must read 8 MiB and formatting a pre-existing volume needs
  `--format-blank`.
- `tools/do_vm.py test` still fails closed naming
  `crates/slipfork-engine/tests/critical_btrfs_live.rs` (`M3.14`) and
  `crates/slipfork/tests/critical_btrfs_live.rs` (`M3.15`) until those suites land; the
  design-of-record `test` doctor preflight remains for `M3.19`.
