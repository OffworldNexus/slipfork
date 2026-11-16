---
id: M3.15
title: "slipforkd BtrfsEngine wiring, slipfork doctor Btrfs probe, daemon unit --only apparmor userns profile"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.13]
paths: ["crates/slipfork/src/**", "crates/slipfork/tests/**", crates/slipfork/Cargo.toml]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T16:58"
---

# Goal

Wire `slipforkd` to the single `BtrfsEngine`, make `slipfork doctor` probe Btrfs magic + a live subvolume ioctl round-trip (failing closed with the exact mount command), add the `/etc/apparmor.d/slipfork` `userns` profile rendering, and delete every `incus` field/argument from the CLI, daemon, and its critical suites.

# Context

- `slipforkd` is the sole Composition Root (`H-010`): it constructs the engine once, so the driver/engine choice lives there and nowhere else.
- Ubuntu 24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1`; without the profile every unprivileged world `unshare` is denied (`EPERM`) and the failure looks like a bug in Slipfork.
- `crates/slipfork/tests/critical_l0_wiring.rs` boots real worlds through a real `slipforkd`, so it splits into driver-injected daemon assertions plus a live suite, exactly like `M3.14`.

# Design / Invariants

1. `slipforkd` builds `KernelBtrfsDriver` + `BtrfsEngine` from explicit `--state-dir`, surfacing `EngineError::BtrfsRequired` (with the mount hint) at boot before any world directory exists. Zero `match engine`, zero `cfg`-selectable alternate engine, zero `incus` field in `DaemonStatus`/`doctor` JSON.
2. `slipfork doctor` reports, from real probes: `filesystem` (from `statfs`), `btrfs_super_magic: ok|mismatch`, `subvolume_ioctl: ok|<errno>` (create, snapshot, clear-RO, destroy in a scratch subvolume under `<state_dir>`), `netns`, `crun`, and `unshare_userns` (including an AppArmor-restriction hint when `EPERM`). Non-Btrfs exits 1 with the exact `mount -o user_subvol_rm_allowed,noatime` fix and prints nothing that implies a fallback.
3. `slipfork daemon unit --only socket|service|apparmor` renders the 4-line `/etc/apparmor.d/slipfork` profile (`userns,` plus the two required sub-rules per `ADR-0010`) alongside the existing non-template unit rendering; `--only apparmor` prints only the profile on stdout.
4. `crates/slipfork/tests/`: `critical_l0_wiring.rs` and friends use a crate-local test driver through an injected engine for daemon-level assertions; `tests/critical_btrfs_live.rs` (new) runs the real `slipforkd` + Btrfs + `crun` + `postgres:18` path on a Btrfs host and asserts `BtrfsRequired` (exit 1 plus hint text) on a non-Btrfs host. No `#[ignore]`.
5. Published docs and `AGENTS.md` are **not** in this node (`M3.18`).

# Acceptance

```bash
cargo test -p slipfork --all-targets
cargo clippy -p slipfork --all-targets -- -D warnings
! grep -rni 'incus' crates/slipfork/src/ crates/slipfork/tests/
cargo run -q -p slipfork -- daemon unit --only apparmor | grep -n 'userns'
cargo run -q -p slipfork -- doctor; test $? -eq 1
cargo run -q -p slipfork -- doctor 2>&1 | grep -n 'user_subvol_rm_allowed,noatime'
grep -n 'btrfs_super_magic' crates/slipfork/src/main.rs
make rs-test
```

Expected: `cargo test --all-targets` and clippy exit 0; the inverted `grep` prints nothing; `daemon unit --only apparmor` prints the profile containing `userns`; on this XFS host `doctor` exits **1** and its output names `mount -o user_subvol_rm_allowed,noatime`; `btrfs_super_magic` appears in the doctor fields; `make rs-test` exits 0.
