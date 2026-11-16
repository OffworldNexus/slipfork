---
id: M3.21
title: "Atomic scorched-earth deletion: remove IncusEngine, ReflinkNetnsEngine, and FICLONE helpers"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.14, M3.15]
paths:
  - "crates/slipfork-engine/src/incus.rs"
  - "crates/slipfork-engine/src/reflink_netns.rs"
  - "crates/slipfork-engine/src/linux_cow.rs"
  - "crates/slipfork-engine/src/lib.rs"
  - "crates/slipfork-engine/Cargo.toml"
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T17:25"
updated: "2026-11-16T17:25"
---

# Goal

Atomically delete `IncusEngine`, `ReflinkNetnsEngine`, and every `FICLONE` helper/error from `crates/slipfork-engine` once `M3.14` (`slipfork-engine` suites + `support/integrations.toml`) and `M3.15` (`slipfork` daemon/CLI/suites) have migrated onto `BtrfsEngine` — leaving `BtrfsEngine` as the sole `WorldEngine` in the workspace with `make check` green.

# Context

- `M3.13` built `BtrfsEngine`, `M3.14` re-homed `crates/slipfork-engine/tests/**` + `support/integrations.toml`, and `M3.15` re-homed `crates/slipfork/**` onto `BtrfsEngine`. Zero callers of `IncusEngine`, `ReflinkNetnsEngine`, or `linux_cow::{reflink_file, reflink_tree, probe_reflink_dir, ReflinkStats}` remain anywhere in `crates/` or `packages/`.
- Performing the physical deletion here keeps `make check` 100% green at every single commit across `M3.12` -> `M3.21` while enforcing Iron Law #3 (zero dead engines, zero `FICLONE` helpers, zero fallback residue).

# Design / Invariants

1. Delete `crates/slipfork-engine/src/incus.rs` and `crates/slipfork-engine/src/reflink_netns.rs` and remove their `mod` / `pub use` lines from `crates/slipfork-engine/src/lib.rs`.
2. Delete `FICLONE_IOCTL`, `ReflinkStats`, `reflink_file`, `reflink_dir_inner`, `reflink_tree`, `probe_reflink_dir`, and `EngineError::ReflinkUnsupported` from `crates/slipfork-engine/src/{linux_cow.rs,lib.rs}`; keep only the audited `current_uid_gid`, `ProcessSignal`/kill, and `send_fd`/`recv_fd` helpers in `linux_cow.rs`.
3. Update `crates/slipfork-engine/Cargo.toml` package description and `crates/slipfork-engine/src/lib.rs` crate rustdoc to describe `BtrfsEngine` (`ADR-0010`) exclusively.

# Acceptance

```bash
test ! -e crates/slipfork-engine/src/incus.rs
test ! -e crates/slipfork-engine/src/reflink_netns.rs
! grep -rniE 'incus|reflink|ficlone|statefulram|snapshotmode|zfs' crates/ packages/
cargo clippy --workspace --all-targets -- -D warnings
make check
```

Expected: both `test ! -e` pass; the workspace-wide inverted `grep` across `crates/` and `packages/` prints nothing; clippy and `make check` exit 0.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
