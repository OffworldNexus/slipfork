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
