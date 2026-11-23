---
id: M3.12
title: "SubvolumeDriver seam: KernelBtrfsDriver (statfs + BTRFS_IOC_*) and test-only tree driver"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.8, M3.9, M3.20]
paths:
  - ARCHITECTURE.md
  - "crates/slipfork-engine/src/btrfs.rs"
  - "crates/slipfork-engine/src/lib.rs"
  - "crates/slipfork-engine/Cargo.toml"
  - "crates/slipfork-testkit/**"
  - Cargo.lock
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-17T04:43"
---

# Goal

Add the `SubvolumeDriver` seam and its two implementations: `KernelBtrfsDriver` (real `statfs` magic check + `BTRFS_IOC_*` ioctls, the only production driver) and `TestTreeDriver` (plain-directory emulation for fast crate-internal unit tests, reachable only under `#[cfg(test)]`).

# Context

- This container's rootfs is XFS with no `/dev/btrfs-control`, so kernel Btrfs cannot run here; real kernel verification happens on the Btrfs DO VM (`M3.19`). The driver seam keeps `BtrfsEngine` unit-testable without inventing a production non-Btrfs path.
- `crates/slipfork-engine` already keeps all `unsafe` in one audited module; the new ioctls must follow the same discipline.

# Design / Invariants

1. `src/btrfs.rs` defines `pub trait SubvolumeDriver: Send + Sync` with only the capabilities `BtrfsEngine` needs, exactly as `ARCHITECTURE.md` §3.3 and `ADR-0010` §§4–5 specify: `ensure_supported(&Path) -> Result<(), EngineError>` (fail-closed `BTRFS_SUPER_MAGIC` probe), `probe(&Path) -> Result<BtrfsProbeReport, EngineError>` (the single probe seam; `crates/slipfork/src/main.rs` constructs `KernelBtrfsDriver` and passes `&dyn SubvolumeDriver` to `btrfs::probe_with`), `create_subvolume`, `snapshot_ro`, `snapshot_writable`, `destroy_subvolume` (clears `BTRFS_SUBVOL_RDONLY` internally — there is no `set_readonly` method and no capability/kind enum), `list_children`. Names/types may be refined, but every method must be honest: no boolean `flags` bag, no silent retries. Production wiring is pure constructor DI: `BtrfsEngine::new(...)` uses `KernelBtrfsDriver`, `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)` injects; test fakes live in `#[cfg(test)]` / `tests/support/` per `ARCHITECTURE.md` §1 (zero Cargo features).
2. `KernelBtrfsDriver` performs `statfs` (`BTRFS_SUPER_MAGIC = 0x9123683e`), `BTRFS_IOC_SNAP_CREATE_V2` with `BTRFS_SUBVOL_RDONLY` for RO snapshots, `BTRFS_IOC_SNAP_CREATE_V2` for writable clones, `BTRFS_IOC_SUBVOL_SETFLAGS` (clear RO) + `BTRFS_IOC_SNAP_DESTROY_V2` for destroy. Every `unsafe` ioctl lives here with a per-call `// SAFETY:` proof; nothing else in the crate gains `unsafe`.
3. Fail closed: a non-Btrfs `f_type` returns `EngineError::BtrfsRequired { path, mount_hint }` whose `mount_hint` is exactly `mount -o user_subvol_rm_allowed,noatime ...`; no `mkdir`-and-continue, no OverlayFS fallback.
4. `TestTreeDriver` is `#[cfg(test)]` only (crate-internal). It emulates the same observable contract with real directories so unit tests can assert ordering/fail-closed behaviour; production builds contain no reference to it.
5. Unit tests in `src/btrfs.rs`: magic classification (Btrfs vs other types via injected `f_type` values), RO-flag-cleared-before-destroy ordering on the test driver, ENAMETOOLONG/`EROFS`/`ENOTEMPTY` error mapping, and `mount_hint` text.
6. `lib.rs` gains `pub mod btrfs;` and re-exports the trait/types. The crate still compiles against the existing engines (deletions happen in `M3.13`).

# Acceptance

```bash
cargo check -p slipfork-engine
cargo test -p slipfork-engine --lib btrfs
cargo clippy -p slipfork-engine --all-targets -- -D warnings
grep -n 'BTRFS_SUPER_MAGIC' crates/slipfork-engine/src/btrfs.rs
grep -n 'BTRFS_IOC_SNAP_CREATE_V2' crates/slipfork-engine/src/btrfs.rs
grep -n 'BTRFS_IOC_SUBVOL_SETFLAGS' crates/slipfork-engine/src/btrfs.rs
grep -n 'BTRFS_IOC_SNAP_DESTROY_V2' crates/slipfork-engine/src/btrfs.rs
grep -n 'BtrfsRequired' crates/slipfork-engine/src/btrfs.rs
grep -n 'user_subvol_rm_allowed,noatime' crates/slipfork-engine/src/btrfs.rs
grep -c 'SAFETY:' crates/slipfork-engine/src/btrfs.rs
```

Expected: `cargo check`, the filtered `cargo test --lib btrfs`, and clippy all exit 0; all six `grep`s find their symbol; the `SAFETY:` count is >= the number of unsafe ioctl calls (non-zero).

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
