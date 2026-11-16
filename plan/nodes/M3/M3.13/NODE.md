---
id: M3.13
title: "BtrfsEngine implementation: unified per-world Btrfs subvolume, RO snapshots, writable clones, --ipc"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.12]
paths:
  - "crates/slipfork-engine/src/btrfs_engine.rs"
  - "crates/slipfork-engine/src/lib.rs"
  - "crates/slipfork-engine/src/service.rs"
  - "crates/slipfork-engine/src/world_init.rs"
  - "crates/slipfork-engine/src/bin/slipfork-world-init.rs"
  - "crates/slipfork-engine/Cargo.toml"
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T17:25"
---

# Goal

Implement `BtrfsEngine` against the `SnapshotMode`-free `WorldEngine` trait (`M3.20`): a single unified Btrfs subvolume per world holding the `L0` rootfs tree *and* `/var/lib/slipfork/state/{pgdata,redis,s3}`, cloned/snapshotted/destroyed through the injected `SubvolumeDriver` (`M3.12`), and launched with `--ipc` — keeping `make check` green while `M3.14` and `M3.15` migrate callers before `M3.21`'s atomic deletion of `incus.rs`, `reflink_netns.rs`, and `FICLONE`.

# Context

- `M3.12` added `SubvolumeDriver` + `KernelBtrfsDriver`; `M3.20` already removed `SnapshotMode`/`StatefulRam` from `trait WorldEngine` and decoupled `slipfork-edge`. `ADR-0010`/`ARCHITECTURE.md` (`M3.7`) are normative for the layout and ioctl sequence.
- Port the supervision logic (`unshare` spawn, `netns.sock` dial, `crun` service spawn/stop/logs, `exec_in_service`/`exec_in_world`, `entry_command`/`service_entry_command`, `destroy_world` reaping) cleanly into `src/btrfs_engine.rs` (sharing pure supervision helpers in `service.rs`/`world_init.rs` so zero logic is duplicated). Physical deletion of `incus.rs`, `reflink_netns.rs`, and `linux_cow` `FICLONE` helpers happens atomically in `M3.21` immediately after `M3.14` (`slipfork-engine/tests`) and `M3.15` (`slipfork`) switch over, so `make check` stays 100% green on every commit.

# Design / Invariants

1. `src/btrfs_engine.rs` implements `WorldEngine` with:
   - construction (`BtrfsEngine::new` with `KernelBtrfsDriver` and `BtrfsEngine::with_driver` for injected `SubvolumeDriver`) that probes `statfs` on `<state_dir>` through the driver and fails closed `EngineError::BtrfsRequired` (naming `mount -o user_subvol_rm_allowed,noatime`) before creating anything;
   - `clone_world` creating one writable world subvolume from the RO fixture/base snapshot, materialising `<subvol>/rootfs` (from `WorldSpec.rootfs.app_rootfs`, still read-only cached) and `<subvol>/state/{pgdata,redis,s3}` inside that same subvolume;
   - `create_snapshot`/`freeze_world` sealing the whole world subvolume RO via `SubvolumeDriver::snapshot_ro` (`BTRFS_IOC_SNAP_CREATE_V2` + `BTRFS_SUBVOL_RDONLY`; freeze stops `WorldProcess` services first, then `OciContainer` services, then seals);
   - `destroy_world` = stop services, SIGTERM/SIGKILL the persistent init, clear `BTRFS_SUBVOL_RDONLY` (`set_readonly(false)`), then `destroy_subvolume` (`BTRFS_IOC_SNAP_DESTROY_V2`);
   - the ported supervision methods, `netns.sock` `dial_world_tcp`, and the in-netns `http-ingress.sock` bridge.
2. The `unshare` argv gains `--ipc` next to `--mount --net --pid --fork --mount-proc --user --map-user=$UID --map-group=$GID --keep-caps`, with a comment stating the SysV `shmget` key collision it prevents. `src/bin/slipfork-world-init.rs`'s header doc matches the new argv.
3. Re-export `BtrfsEngine` from `src/lib.rs` with unit tests exercising `BtrfsEngine::with_driver` over `TestTreeDriver` (layout, RO freeze, sibling clone isolation, clear-RO-before-destroy, and `BtrfsRequired` refusal).
4. Zero non-Btrfs production path: no `cfg(feature)` selectable engine, no `match filesystem`, no "try X then Y".

# Acceptance

```bash
cargo check --workspace --all-targets
cargo test -p slipfork-engine --lib btrfs_engine
cargo clippy --workspace --all-targets -- -D warnings
! grep -rn 'FICLONE\|reflink_tree\|reflink_file\|IncusEngine\|ReflinkNetnsEngine\|StatefulRam' crates/slipfork-engine/src/btrfs_engine.rs
grep -n -- '"--ipc"' crates/slipfork-engine/src/btrfs_engine.rs
grep -n 'BtrfsRequired' crates/slipfork-engine/src/lib.rs crates/slipfork-engine/src/btrfs_engine.rs
grep -n 'pub use btrfs_engine::BtrfsEngine' crates/slipfork-engine/src/lib.rs
make check
```

Expected: workspace check, `btrfs_engine` unit tests, clippy, and full `make check` exit 0; `btrfs_engine.rs` contains zero reflink/Incus/`StatefulRam` references; `--ipc`, `BtrfsRequired`, and the `BtrfsEngine` re-export are present.
