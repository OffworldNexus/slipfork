---
id: M3.14
title: "Engine conformance + critical suites on BtrfsEngine (driver-injected fast suites, live kernel suite for the VM)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.13]
paths:
  - "crates/slipfork-engine/src/conformance.rs"
  - "crates/slipfork-engine/tests/**"
  - support/integrations.toml
  - "docs/support-matrix.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T17:25"
---

# Goal

Re-home the `slipfork-engine` conformance and critical suites onto `BtrfsEngine` so `make check` is deterministic on a non-Btrfs host **and** the real kernel path is exercised on the Btrfs DO VM — with no `#[ignore]`, no silent skip, and no production non-Btrfs fallback.

# Context

- `M3.13` deleted `IncusEngine`/`ReflinkNetnsEngine`; `crates/slipfork-engine/tests/` still imports them, so `cargo test -p slipfork-engine --all-targets` is red.
- Host capability differs: this host is XFS (no Btrfs), the DO VM (`M3.16`/`M3.19`) has a real Btrfs volume. Tests must assert the *contract* in both environments instead of pretending the filesystem exists.

# Design / Invariants

1. `tests/support/test_tree_driver.rs` implements the public `SubvolumeDriver` over real scratch directories (fixture material, no kernel ioctl) and is shared by every fast suite. It is test-only code; production binaries contain no reference to it.
2. Fast suites (`critical_engine.rs`, `critical_oci_runtime.rs` where kernel-independent) construct `BtrfsEngine::with_driver(test_tree_driver)` and assert: unified single-subvolume layout, RO snapshot isolation between sibling clones, state-subdir lineage, mount-spec validation, opaque-handle stability, seal-once freeze, `$UID:$GID` ownership of world trees, and fail-closed errors.
3. `tests/critical_btrfs_live.rs` (new) probes `statfs(<state_dir>).f_type == 0x9123683e` once:
   - **Btrfs host:** runs the real ioctl path — RO snapshot, two writable clones, `postgres:18` + `redis:8` `crun` services in both clones on the same in-world port, SysV-IPC isolation across clones (proves `--ipc`), and clear-RO-then-destroy teardown.
   - **Non-Btrfs host:** asserts `EngineError::BtrfsRequired` with the `mount -o user_subvol_rm_allowed,noatime` hint, and prints one diagnostic line that the real kernel suite runs on the DO VM. No `#[ignore]`, no early `return` before an assertion.
4. `conformance.rs` documents RO-snapshot-vs-writable-clone as distinct operations (`SnapshotMode` was removed in `M3.20`).
5. Update any `slipfork_engine::*` `critical_test` function names in `support/integrations.toml` (renaming `reflink-netns-world-engine` -> `btrfs-world-engine`) and run `make support-sync` so `make support-check` / `make check` stays green. Keep every existing invariant assertion; deleting an assertion requires a matching replacement in the same node.

# Acceptance

```bash
cargo test -p slipfork-engine --all-targets
cargo clippy -p slipfork-engine --all-targets -- -D warnings
grep -n '0x9123683e\|BTRFS_SUPER_MAGIC' crates/slipfork-engine/tests/critical_btrfs_live.rs
grep -n 'BtrfsRequired' crates/slipfork-engine/tests/critical_btrfs_live.rs
grep -n 'postgres:18' crates/slipfork-engine/tests/critical_btrfs_live.rs
! grep -rn 'ReflinkNetnsEngine\|IncusEngine' crates/slipfork-engine/tests/
! grep -rn '#\[ignore' crates/slipfork-engine/tests/
make rs-test
```

Expected: `cargo test --all-targets` and clippy exit 0 on this XFS host (fast suites pass; the live suite asserts the fail-closed contract); the three `grep`s find the magic value, the error, and the real `postgres:18` service spec; both inverted `grep`s print nothing; `make rs-test` exits 0.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
