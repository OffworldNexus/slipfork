---
id: M3.10
title: "Rewrite docs/design/do-vm-harness.md: Btrfs volume, crun, AppArmor userns, persistent --identity"
kind: task
status: ready
owner: designer
priority: 2
depends_on: [M3.7]
paths: ["docs/design/do-vm-harness.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T16:58"
---

# Goal

Rewrite `docs/design/do-vm-harness.md` for the Btrfs + `crun` harness: a DigitalOcean droplet + attached block volume formatted **Btrfs** at `/var/lib/slipfork` (`-o user_subvol_rm_allowed,noatime`), with `btrfs-progs`, `crun`, `docker.io`/`docker-buildx`, a Rust toolchain, and an AppArmor `userns` profile — plus a persistent SSH identity so a `/tmp` reboot never loses access.

# Context

- `M3.5` shipped the harness design for a ZFS pool + Incus; that substrate is deleted (owner ruling), and the live verification suites now need a real Btrfs kernel + `crun` + `postgres:18`.
- `docs/design/do-vm-harness.md` is internal (`exclude_docs`), so it must match the shipped `tools/do_vm.py` exactly — this doc is the design the `M3.16` implementer executes.

# Design / Invariants

1. Commands stay exactly `up | status | sync | test | ssh | down`, plus the new repeatable `--identity <path>` on `up`/`ssh`/`status`/`test` defaulting to a **persistent** `~/.cache/slipfork/do-vm/id_ed25519` (auto-generated + imported into the DO account when absent). State the failure mode that motivated it: a key under `/tmp` disappears on reboot, locking the owner out of a still-billing droplet.
2. Cloud-init provisioning (idempotent, bounded): `apt-get install btrfs-progs crun docker.io docker-buildx openssh-server`; `mkfs.btrfs -f -L slipfork /dev/disk/by-id/scsi-0DO_Volume_*` when the device has no Btrfs signature; mount at `/var/lib/slipfork` with `user_subvol_rm_allowed,noatime` via `/etc/fstab`; verify `statfs` magic `0x9123683e`; install the Rust toolchain (rustup, edition 2024) so `do_vm.py test` never depends on a manual prerequisite; write `/etc/apparmor.d/slipfork` with the 4-line `userns` profile and reload it.
3. Document that no nested KVM exists on DigitalOcean and that nothing here claims VM isolation.
4. `test` streams the real-kernel suites: `cargo test -p slipfork-engine --test critical_btrfs_live -- --test-threads=1` and `cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1`, failing closed naming the missing suite while it does not exist yet.
5. Cost model and `up`-never-destroys behaviour unchanged; delete every ZFS/Incus statement and the `incus-ready` marker.

# Acceptance

```bash
! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md
grep -n 'mkfs.btrfs' docs/design/do-vm-harness.md
grep -n 'user_subvol_rm_allowed,noatime' docs/design/do-vm-harness.md
grep -n -- '--identity' docs/design/do-vm-harness.md
grep -n 'apparmor' docs/design/do-vm-harness.md
grep -n 'critical_btrfs_live' docs/design/do-vm-harness.md
make docs-check
```

Expected: the inverted `grep` prints nothing; the five `grep`s find `mkfs.btrfs`, the mount options, `--identity`, the AppArmor profile, and the live suite names; `make docs-check` exits 0.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
