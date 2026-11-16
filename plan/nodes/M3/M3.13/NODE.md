---
id: M3.13
title: "BtrfsEngine: sole production WorldEngine (unified per-world subvolume, RO snapshots, writable clones, --ipc); delete IncusEngine, ReflinkNetnsEngine and FICLONE"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.12]
paths:
  - "crates/slipfork-engine/src/btrfs_engine.rs"
  - "crates/slipfork-engine/src/lib.rs"
  - "crates/slipfork-engine/src/linux_cow.rs"
  - "crates/slipfork-engine/src/incus.rs"
  - "crates/slipfork-engine/src/reflink_netns.rs"
  - "crates/slipfork-engine/src/service.rs"
  - "crates/slipfork-engine/src/world_init.rs"
  - "crates/slipfork-engine/src/conformance.rs"
  - "crates/slipfork-engine/src/bin/slipfork-world-init.rs"
  - "crates/slipfork-engine/Cargo.toml"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T16:58"
---

# Goal

Replace both shipped engines with **one** production `BtrfsEngine`: a single unified Btrfs subvolume per world holding the `L0` rootfs tree *and* `/var/lib/slipfork/state/{pgdata,redis,s3}`, cloned/snapshotted/destroyed through the injected `SubvolumeDriver`, launched with `--ipc` — and delete `incus.rs`, `reflink_netns.rs`, and every `FICLONE` helper.

# Context

- `M3.12` added `SubvolumeDriver` + `KernelBtrfsDriver`. `ADR-0010`/`ARCHITECTURE.md` (`M3.7`) are normative for the layout and ioctl sequence.
- `reflink_netns.rs` (1900 lines) contains the supervision logic to port: `unshare` spawn, `netns.sock` dial, `crun` service spawn/stop/logs, `exec_in_service`/`exec_in_world`, `entry_command`/`service_entry_command`, `destroy_world` reaping. `incus.rs` (518 lines) is deleted outright.
- This port is one atomic compile unit: a half-ported `impl WorldEngine` cannot build, so it must land as one node even though it moves more than 500 lines (net LOC is negative: ~2400 lines deleted, ~700 moved).

# Design / Invariants

1. `src/btrfs_engine.rs` implements `WorldEngine` with:
   - construction that probes `statfs` on `<state_dir>` through the driver and fails closed `EngineError::BtrfsRequired` (naming `mount -o user_subvol_rm_allowed,noatime`) before creating anything;
   - `clone_world` creating one writable world subvolume from the RO fixture/base snapshot, materialising `<subvol>/rootfs` (from `WorldSpec.rootfs.app_rootfs`, still read-only cached) and `<subvol>/state/{pgdata,redis,s3}` inside that same subvolume;
   - `create_snapshot`/`freeze_world` sealing the whole world subvolume RO via `BTRFS_IOC_SNAP_CREATE_V2` + `BTRFS_SUBVOL_RDONLY` (freeze stops `WorldProcess` services first, then `OciContainer` services, then seals);
   - `destroy_world` = stop services, SIGTERM/SIGKILL the persistent init, clear `BTRFS_SUBVOL_RDONLY` via `BTRFS_IOC_SUBVOL_SETFLAGS`, then `BTRFS_IOC_SNAP_DESTROY_V2`;
   - the ported supervision methods, `netns.sock` `dial_world_tcp`, and the in-netns `http-ingress.sock` bridge.
2. The `unshare` argv gains `--ipc` next to `--mount --net --pid --fork --mount-proc --user --map-user=$UID --map-group=$GID --keep-caps`, with a comment stating the SysV `shmget` key collision it prevents. `src/bin/slipfork-world-init.rs`'s header doc matches the new argv.
3. Delete `src/incus.rs`, `src/reflink_netns.rs`, `linux_cow::{reflink_file, reflink_tree, ReflinkStats, probe_reflink_dir}`; `linux_cow.rs` keeps only the audited uid/gid, `kill(2)`, and `send_fd`/`recv_fd` helpers.
4. Delete `SnapshotMode` (and `StatefulRam`) from the public API; RO snapshot vs writable clone are separate methods. Update `conformance.rs`, `service.rs`, `world_init.rs`, `lib.rs` re-exports and `crates/slipfork-engine/Cargo.toml`'s description accordingly.
5. Zero non-Btrfs production path: no `cfg(feature)` selectable engine, no `match filesystem`, no "try X then Y".

# Acceptance

```bash
cargo check -p slipfork-engine
cargo test -p slipfork-engine --lib
cargo clippy -p slipfork-engine -- -D warnings
test ! -e crates/slipfork-engine/src/incus.rs
test ! -e crates/slipfork-engine/src/reflink_netns.rs
! grep -rn 'FICLONE\|reflink_tree\|reflink_file\|IncusEngine\|ReflinkNetnsEngine\|StatefulRam' crates/slipfork-engine/src/
grep -n -- '"--ipc"' crates/slipfork-engine/src/btrfs_engine.rs
grep -n 'BtrfsRequired' crates/slipfork-engine/src/lib.rs crates/slipfork-engine/src/btrfs_engine.rs
grep -n 'BTRFS_IOC_SNAP_DESTROY_V2' crates/slipfork-engine/src/btrfs_engine.rs
```

Expected: lib check, lib tests, and clippy exit 0; both `test ! -e` succeed; the inverted `grep` prints nothing; `--ipc`, `BtrfsRequired`, and the destroy ioctl are present. (`--all-targets` is *not* expected to pass yet — the integration suites are `M3.14`.)
