---
id: M3.39.1
title: "Pure Btrfs layout: host probe, transparent subvolume conversion, RO image/world store, CoW fork & safe destroy"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-world/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T01:53"
---

# Goal
Give `slipfork-world` the real `WorldEngine`/`BtrfsEngine` core: verify `BTRFS_SUPER_MAGIC`, convert a plain checkout into a project subvolume once, own `.slipfork-store/{images,worlds}`, seal read-only subvolumes, and provide sub-50ms CoW fork + destroy for project and world data.

# Context
- Layout + tree + fork steps: `specs/03-under-the-hood.md:13-30` and `:49-53`; store paths `.slipfork-store/images` / `.slipfork-store/worlds`.
- Crate/port ownership (this crate owns `WorldEngine` -> `BtrfsEngine`, no other crate does Btrfs ioctls): `specs/03-under-the-hood.md:116-131`; `slipfork-engine` must not exist.
- Replace the placeholder marker in `crates/slipfork-world/src/lib.rs:1-13`; crate `Cargo.toml` currently has an empty `[dependencies]` (`crates/slipfork-world/Cargo.toml:1-7`).
- Kernel/ioctl facts that must drive the implementation: no reflink/CoW on `/tmp` tmpfs (`.opencode/LESSONS.md:5`), seal with `BTRFS_SUBVOL_RDONLY` and clear the flag before `BTRFS_IOC_SNAP_DESTROY_V2` (`.opencode/LESSONS.md:16-17`), unprivileged delete needs `-o user_subvol_rm_allowed` (same line).
- Composition root is only `crates/slipfork/src/main.rs`; this crate stays env-free and takes absolute paths/handles as constructor arguments. No `std::env::var`, no `OnceLock`/`LazyLock`, no static mutable state.
- Acceptance for the epic: `plan/nodes/M3/M3.39/NODE.md` ("`< 50ms` atomic CoW fork of BOTH project and world data subvolumes, plus dirty-checkout refusal on `drop` without `--force`").
- Delete, do not keep, any fallback that pretends to work on non-Btrfs: probe and fail closed with the `mount -o user_subvol_rm_allowed,noatime` remedy.

# Acceptance
- `cargo test -p slipfork-world`
- Protected lab test `crates/slipfork-world/tests/critical_btrfs_live.rs` uses a real Btrfs directory (`~/.cache/slipfork` or `/var/tmp`, never `/tmp`), asserts: `BTRFS_SUPER_MAGIC` probe; conversion of a plain dir to subvolume; sealed subvolume write rejection (`BTRFS_SUBVOL_RDONLY`, not a mode bit); destroy of a sealed subvolume requires flag-clear and succeeds; fork of project + world data completes in under 50ms wall clock; dirty checkout refuses `drop` without `--force`. It must fail closed (panic with a clear message), never skip, on non-Btrfs.
- Zero `Mock*`, `Fake*`, `Stub*`, `Recording*`, or `TestTreeDriver` symbols anywhere under `crates/slipfork-world`.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `uv run --no-project tools/guard.py`
