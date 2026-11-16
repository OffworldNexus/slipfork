# Log M3.16

## 2026-11-16T16:58 [planner]

created (task) tools/do_vm.py: Btrfs volume, crun/docker/Rust, AppArmor userns, persistent --identity (zero ZFS/Incus)

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T18:59 [lead]

REQUIREMENT (from M3.7 legality review): /etc/apparmor.d/slipfork userns profile must explicitly allow /usr/bin/unshare (and slipfork-world-init) to create unprivileged user namespaces under kernel.apparmor_restrict_unprivileged_userns=1; verify live on the DO VM.
