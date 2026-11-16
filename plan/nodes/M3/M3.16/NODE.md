---
id: M3.16
title: "tools/do_vm.py: Btrfs volume, docker/Rust, AppArmor userns, persistent --identity (zero ZFS/Incus)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.10, M3.15]
paths: [tools/do_vm.py, tools/tests/test_do_vm.py]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T21:44"
---

# Goal

Rewrite `tools/do_vm.py` (and its unit tests) for the Btrfs + `crun` substrate: provision the DO block volume with `mkfs.btrfs` at `/var/lib/slipfork` (`-o user_subvol_rm_allowed,noatime`), install `btrfs-progs` + `docker.io`/`docker-buildx` + Rust (no host `crun`: it ships inside the `L0` world rootfs), install the AppArmor `userns` profile, add a **persistent** `--identity` SSH key, and run the real Btrfs kernel suites remotely — with zero ZFS and zero Incus.

# Context

- `M3.5` shipped this harness for ZFS + Incus; that substrate is deleted. `docs/design/do-vm-harness.md` (`M3.10`) is the design of record for this node.
- Live verification (`M3.19`) depends on this harness booting a real Btrfs world on a remote host.

# Design / Invariants

1. Keep the six commands (`up`, `status`, `sync`, `test`, `ssh`, `down`), the tag-scoped idempotent find-or-create, `--yes` guards, bounded waits, and fail-closed `doctl` handling. Add repeatable `--identity <path>` (default `~/.cache/slipfork/do-vm/id_ed25519`, auto-generated when absent and imported into the account when `doctl` cannot see it) used by every SSH call.
2. Provisioning is one idempotent script: install `btrfs-progs docker.io docker-buildx` and mount the volume (no host `crun`: the static `crun` ships inside the `L0` world rootfs per `ADR-0009`; `doctor` performs no `crun` probe and emits no `crun` field at all (`ADR-0010` §4)).
   `mkfs.btrfs -f` only when the attached volume has no Btrfs signature — branch on the `blkid` exit status (`0` use the value, `2` blank device → format, anything else fail closed naming device, rc, and stderr; never `|| true`); mount at `/var/lib/slipfork` with `user_subvol_rm_allowed,noatime` in the fstab entry; verify `statfs` magic `0x9123683e`.
   Install rustup + Rust >= 1.94 (edition 2024); write `/etc/apparmor.d/slipfork` (4-line `userns` profile permitting `/usr/bin/unshare` and `slipfork-world-init`) and reload it; write the readiness marker at `/var/lib/slipfork/.slipfork-ready` (inside the mount, so a failed remount hides it and `up` fails closed). `status`/`test`/`ssh` accept the same repeatable `--identity`, and `ssh` forwards a trailing remote command (`ssh -- <cmd>`).
3. `test` streams `cargo test -p slipfork-engine --test critical_btrfs_live` then `cargo test -p slipfork --test critical_btrfs_live`, both with serialized threads, failing closed naming the missing file while it does not exist.
4. `sync` keeps its `.env` refusal and `tar`-over-SSH transport (no rsync). No account-specific literals, no `shell=True`, stdlib only with the `# /// script` block.
5. Delete every ZFS/Incus code path, flag, marker file, and docstring from the tool.

# Acceptance

```bash
uv run --no-project tools/do_vm.py --help
uv run --no-project tools/do_vm.py up --help
! grep -rniE 'zfs|zpool|incus|zfsutils' tools/do_vm.py
grep -n 'mkfs.btrfs' tools/do_vm.py
grep -n 'user_subvol_rm_allowed,noatime' tools/do_vm.py
grep -n 'identity' tools/do_vm.py
grep -n 'apparmor' tools/do_vm.py
uv run --no-project --with pytest pytest -q tools/tests
uvx --offline ruff check tools && uvx --offline ruff format --check tools
! grep -rn 'dogeops\|DogeOps\|60390733' tools/do_vm.py
```

Expected: both `--help` invocations exit 0 and list the six commands plus `--identity`; the inverted `grep`s print nothing; the four `grep`s find `mkfs.btrfs`, the mount options, `identity`, and the AppArmor profile; `pytest -q tools/tests` and both `ruff` commands exit 0.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
