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
