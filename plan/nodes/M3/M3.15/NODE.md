---
id: M3.15
title: "slipforkd BtrfsEngine wiring, slipfork doctor Btrfs probe, daemon unit --only apparmor userns profile"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.14]
paths:
  - Makefile
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.lock
  - "crates/slipfork-engine/src/lib.rs"
  - tools/do_vm.py
  - tools/tests/test_do_vm.py
  - "packages/opencode-slipfork/tests/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-17T12:36"
---

# Goal

Wire `slipforkd` to the single `BtrfsEngine`, make `slipfork doctor` probe Btrfs magic + a live subvolume ioctl round-trip (failing closed with the exact mount command), add the `/etc/apparmor.d/slipfork` `userns` profile rendering, and delete every `incus` field/argument from the CLI, daemon, and its critical suites.

# Context

- `slipforkd` is the sole Composition Root (`H-010`): it constructs the engine once, so the driver/engine choice lives there and nowhere else.
- Ubuntu 24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1`; without the profile every unprivileged world `unshare` is denied (`EPERM`) and the failure looks like a bug in Slipfork.
- `crates/slipfork/tests/critical_l0_wiring.rs` boots real worlds through a real `slipforkd`, so it splits into driver-injected daemon assertions plus a live suite, exactly like `M3.14`.

# Design / Invariants

1. `slipforkd` builds `KernelBtrfsDriver` + `BtrfsEngine` from explicit `--state-dir`, surfacing `EngineError::BtrfsRequired` (with the mount hint) at boot before any world directory exists. Zero `match engine`, zero `cfg`-selectable alternate engine, zero `incus` field in `DaemonStatus`/`doctor` JSON.
2. `slipfork doctor` reports, from real probes, through the sanctioned diagnostic seam (`M3.25`): the Btrfs fields of `BtrfsProbeReport` (`filesystem` from `statfs`, `btrfs_super_magic: ok|mismatch`, `subvolume_ioctl: ok|<errno>` — create, snapshot, clear-RO, destroy in a scratch subvolume under `<state_dir>`, plus `user_subvol_rm_allowed`/`noatime` mount-option evidence) via `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, …)`, and the `HostProbe` fields `netns` and `unshare_userns` (including the AppArmor `restrict_unprivileged_userns` `EPERM` hint). `doctor` performs **no** `crun` probe at all and emits **no** `crun` field (`ADR-0010` §4, `M3.9`): the static `crun` is provisioned by `crates/slipfork::runtime::runtime_assets` into `<state_dir>/cache/runtime/` and staged into `WorldRootfs.crun_bin`, never a host `PATH` lookup. `crates/slipfork/src/main.rs` constructs `KernelBtrfsDriver` + `OsHostProbe`; `doctor` holds zero `WorldEngine`. Non-Btrfs exits 1 with the exact `mount -o user_subvol_rm_allowed,noatime` fix and prints nothing that implies a fallback.
3. `--world` accepts the printed qualified id: implement `project::resolve_world_selector` per `ADR-0005` §3 so `<project>`, `<project>:<fork>`, and bare `<fork>` all resolve, and `list`/forward `--json` `world` values use the identical `<project>[:<fork>]` form the console prints.
4. `slipfork daemon unit --only socket|service|apparmor` renders the 4-line `/etc/apparmor.d/slipfork` profile (`userns,` plus the two required sub-rules per `ADR-0010`) alongside the existing non-template unit rendering; `--only apparmor` prints only the profile on stdout.
5. `crates/slipfork/tests/`: in `tests/support/mod.rs`, serve `slipfork::daemon` on `<runtime_dir>/daemon.sock` in-process with `BtrfsEngine::with_driver(test_tree_driver)` on non-Btrfs hosts so `critical_l0_wiring.rs`, `critical_cli.rs`, `critical_ospp_daemon.rs`, `critical_slug.rs`, and `critical_vcs_remote.rs` keep driving the real `slipfork` / `git-remote-slipfork` CLI binaries over `daemon.sock`; `tests/critical_btrfs_live.rs` (new) spawns the real `CARGO_BIN_EXE_slipforkd` binary — running the full `slipforkd` + Btrfs + `crun` + `postgres:18` path on a Btrfs host and asserting `BtrfsRequired` (exit 1 plus hint text) on a non-Btrfs host. No `#[ignore]`.
6. Published docs and `AGENTS.md` are **not** in this node (`M3.18`).

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

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
