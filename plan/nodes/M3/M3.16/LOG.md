# Log M3.16

## 2026-11-16T16:58 [planner]

created (task) tools/do_vm.py: Btrfs volume, crun/docker/Rust, AppArmor userns, persistent --identity (zero ZFS/Incus)

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T18:59 [lead]

REQUIREMENT (from M3.7 legality review): /etc/apparmor.d/slipfork userns profile must explicitly allow /usr/bin/unshare (and slipfork-world-init) to create unprivileged user namespaces under kernel.apparmor_restrict_unprivileged_userns=1; verify live on the DO VM.

## 2026-11-16T21:34 [lead]

M3.25 RULING APPLIED: harness installs btrfs-progs + docker.io/docker-buildx + Rust, NO host crun (static crun ships inside the L0 world rootfs per ADR-0009).

## 2026-11-16T21:43 [lead]

CORRECTION (M3.9 attempt 2): do not implement a world_rootfs_crun doctor field — doctor emits no crun field (ADR-0010 §4). Harness still installs no host crun; the static asset is provisioned by slipforkd runtime_assets.

## 2026-11-16T21:44 [lead]

edited fields

## 2026-11-16T21:45 [lead]

BODY CORRECTION (M3.9 ioc gate): invariant 2 no longer says doctor reports world_rootfs_crun — doctor performs no crun probe and emits no crun field (ADR-0010 §4).

## 2026-11-16T21:52 [lead]

REQUIREMENTS from M3.10 final IoC confirm (implement in do_vm.py): (1) drop the separate aa-status/control-unshare from `test` — the single `runuser -u slipfork -- target/debug/slipfork doctor --json --state-dir /var/lib/slipfork` run is the only host-capability gate; (2) usage synopsis: --identity is repeatable on `up` only, single for status/test/ssh (reject 2nd, exit 2); (3) run the owner ssh envelope under `env -i HOME=… TMPDIR=… PATH=… exec bash --noprofile --norc` so ambient XDG_CACHE_HOME/XDG_DATA_HOME cannot change the resolved state dir and no rc file rewrites PATH; (4) fail closed if slipfork resolves to uid 0 — derive from doctor --json uid field, not a vacuous `id -u` echo; (5) pass a frozen config object into render_* helpers instead of reading/patching module globals.

## 2026-11-16T23:52 [lead]

status ready -> active

## 2026-11-16T23:53 [lead]

attempt 1/3 started

## 2026-11-16T23:53 [lead]

base ca5b19175d139a0b042b6477268c89d231a10a2a

## 2026-11-17T00:40 [lead]

status active -> blocked

## 2026-11-17T00:40 [lead]

BLOCKED (missing-input): volume carries ZFS incuspool, not Btrfs. Static acceptance green (104 pytest, ruff, guard, help, greps). Live proof fails closed at blkid: 'gpt / gpt / PMBR' + /dev/sda1 zfs_member; no mkfs performed. Droplet recreated (620064888) then destroyed by down --yes; volume kept and unattached. H-00x filed for wipe authorization (A) or fresh volume (B).

## 2026-11-17T00:52 [lead]

OWNER ANSWER (H-005, verbatim): 'Wipe ZFS & format Btrfs (Recommended)'. One-time authorized wipe of volume 131a2765-e1ba-11f1-b67b-d6114774f516 (/dev/sda): wipefs -af /dev/sda1 /dev/sda + zero first MiB, then the normal fail-closed up formats Btrfs. do_vm.py's default blkid guard STAYS intact.

## 2026-11-17T00:54 [lead]

status blocked -> ready

## 2026-11-17T00:54 [lead]

status ready -> active

## 2026-11-17T00:54 [lead]

H-005 closed by owner: wipe authorized. Resuming attempt 1 (no bump).

## 2026-11-17T01:32 [lead]

status active -> review

## 2026-11-17T01:32 [lead]

LIVE GREEN after owner-authorized H-005 wipe: /dev/sda Btrfs LABEL=slipfork UUID=c3b7822d-…, findmnt rw,noatime,user_subvol_rm_allowed, statfs 9123683e, marker READY, remy@doc + slipfork-do-vm keys in root+slipfork authorized_keys, rustc/cargo 1.94.0, docker buildx 0.30.1, AppArmor slipfork profile loaded. Droplet 620073485 ams3 165.232.86.174 LEFT UP. Static: 105 pytest, ruff, guard 486 files, plan OK, inverted greps clean. Quad gate starting. Design-doc port items -> M3.23: AppArmor /{a,b} header, ssh -- env, volume -partN discovery.

## 2026-11-17T01:40 [lead]

LEAD RULING (legality BLOCKER, owner-safety): pre-existing-volume formatting now requires explicit --format-blank + proof (blkid rc2, wipefs -n clean, first 8 MiB read-back zero); a volume created by the same up may auto-format; a DETECTED foreign signature always fails closed and --format-blank never overrides it. Default up stays fail-closed. --identity repeatable everywhere, default always materialized, status/test/ssh use the first. test -- <cargo args> deleted.

## 2026-11-17T03:02 [lead]

Gate policy from round 4: stop at the first round with no BLOCKER/MAJOR; remaining MINORs are logged as followups (M3.23 doc sweep / M3.15 byte-equivalence) instead of another code churn cycle, per owner's 'keep leaf reviews crisp' ruling.

## 2026-11-17T03:14 [lead]

status review -> done
