# Log M3.19

## 2026-11-16T16:58 [planner]

created (task) Live verification on the Btrfs DO VM: BtrfsEngine + crun + postgres:18 suites, measured numbers, reboot-safe SSH

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

ADD (M3.1 legality gate): the live VM run must include one make check under deliberately elevated host load (e.g. parallel CPU burners) to prove the Btrfs engine stop path is bounded-but-abort-aware, not grace-window dependent.

## 2026-11-16T20:59 [lead]

REQUIREMENT (M3.8): run `slipfork prune` before asserting an empty `btrfs subvolume list` — the digest-keyed sealed base subvolume survives until GC (world-base-image.md §7.2 #4 / §8.4 #5).

## 2026-11-16T21:03 [lead]

ACCEPTANCE REFINED: teardown asserts `slipfork prune --yes` then `btrfs subvolume list | grep -v /snapshots/` empty — the digest-keyed sealed base under <state_dir>/snapshots survives until the Phase-2 M3.2.13 GC, so a globally empty list is not assertable in Phase 1.

## 2026-11-16T21:34 [lead]

M3.25 RULING APPLIED: host crun --version replaced by in-world slipfork shell -- crun --version.

## 2026-11-16T21:44 [lead]

ACCEPTANCE ORDER: `slipfork up` precedes the in-world `slipfork shell -- crun --version` check (a world must exist for in-world verification).

## 2026-11-17T01:34 [lead]

LIVE VM READY (from M3.16, owner-authorized H-005 wipe): droplet 620073485 'slipfork-dev-vm' ams3, IPv4 165.232.86.174, LEFT UP. Volume 131a2765 Btrfs LABEL=slipfork UUID=c3b7822d-… at /var/lib/slipfork mounted rw,noatime,user_subvol_rm_allowed (statfs 9123683e). SSH: ssh -i ~/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174; authorized_keys for root AND slipfork contain both the do-vm key and remy@doc. Provisioned: rustc/cargo 1.94.0, docker buildx 0.30.1, AppArmor 'slipfork' userns profile loaded, /var/lib/slipfork/.slipfork-ready. Leave it UP; install slipfork+slipforkd on PATH and stage the interactive demo project under /home/slipfork/demo for the owner.

## 2026-11-17T12:28 [lead]

status ready -> active

## 2026-11-17T12:28 [lead]

attempt 1/3 started

## 2026-11-17T12:28 [lead]

base 053596ee33193d5e947e933bf2c0506f366ff3f8

## 2026-11-17T11:41 [implementer]

attempt 1/3 BLOCKED (direction-challenge). Btrfs ioctl path proven live on droplet 620073485: RO snapshot + two writable clones green in slipfork-engine:critical_btrfs_live; acceptance findmnt/stat/prune/subvolume-list/docker buildx all pass. First failing assertion: `clone a: spawn postgres: ... service 'postgres' exited before port 5432 accepted connections` (critical_btrfs_live.rs:741), reproduced by the demo's `slipfork up` as `unshare: Operation not permitted`. Root cause: Ubuntu 24.04 transitions the unprofiled in-world `/.slipfork/bin/crun` into `unprivileged_userns` and denies CAP_SYS_ADMIN (kernel audit), because /etc/apparmor.d/slipfork covers only /usr/bin/unshare + the debug world-init; ADR-0010 §7's "unprofiled children stay under the slipfork profile across execve" claim is false. Engine suite, demo up/fork, and the whole benchmark are blocked by this. Also: `slipfork shell -- crun --version` is not a shipped CLI form (clap exit 2; `slipfork exec --` is the non-interactive entry). Phase 3 done (release binaries incl. slipfork-world-init installed in /usr/local/bin, 0.2.0, both users' default state dir on Btrfs, doctor exit 0). Phase 4 demo staged at /home/slipfork/demo (git commit d96ebd0) with README. Phase 5 reboot verified (SSH identity, mount options, 9123683e, doctor 0). do_vm.py test's root cargo probe needs RUSTUP_HOME (worked around with /root/.rustup symlink; followup for M3.16). make check red only on pre-existing rustdoc private-link in crates/slipfork/src/doctor.rs. Droplet LEFT UP; full evidence in RESULT.md.

## 2026-11-17T12:45 [lead]

status active -> blocked

## 2026-11-17T12:45 [lead]

BLOCKED: direction-challenge filed (H-0xx) — AppArmor unprivileged_userns denies the in-world crun; ADR-0010 §7 mechanism is false. Do not retry until the human rules. Droplet LEFT UP; binaries installed; /home/slipfork/demo staged.

## 2026-11-17T13:11 [lead]

LEAD LIVE EVIDENCE (H-006 pending): with the current profile (unshare+debug world-init) and sysctl=1, crun hits unprivileged_userns: dmesg 'profile=unprivileged_userns comm=crun capability=21 sys_admin DENIED'; demo up fails 'unshare: Operation not permitted'. Setting kernel.apparmor_restrict_unprivileged_userns=0 (persisted /etc/sysctl.d/99-slipfork-devvm.conf) makes the whole demo work TODAY on 165.232.86.174: up -> http://demo.localhost:39055; curl returns {hello:world, secret_len:32, world:demo}; exec; fork -> demo-a.demo.localhost:39055; logs; drop demo:demo-a; prune --dry-run. Root cause is the ADR-0010 §7 mechanism being false for unprofiled unshare-callers (crun). Human must pick A/B/C; the sysctl is the verified dev-VM configuration in the meantime.

## 2026-11-17T13:37 [lead]

ARCHITECT RULING (escalation #22, H-006) received and recorded in ADR-0010 §7 + docs/design/do-vm-harness.md + RESULT §8; M3.19.1 applies it. H-006 stays open in HUMAN.md for the owner's awareness; no further engineering decision is pending.

## 2026-11-17T13:42 [lead]

edited fields

## 2026-11-17T13:42 [lead]

OWNER APPROVED H-006: 'Approve profile list (Recommended)'. Ruling applied by M3.19.1 (apparmor.rs + engine hint + do_vm.py harness clause + cli.md + byte-equivalence test). Next: re-apply the profile on 165.232.86.174 (rm /var/lib/slipfork/.slipfork-ready; do_vm.py up --yes), re-sync, run the 5 live suites, re-install 0.1.0 binaries, and verify /home/slipfork/demo live.
