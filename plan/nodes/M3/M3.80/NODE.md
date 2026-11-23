---
id: M3.80
title: "Never touch the user's checkout: drop up-time adopt, copy plain checkouts on fork"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/**"
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/fork_nonbtrfs.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:10"
updated: "2026-11-24T00:31"
---

# Problem

`slipfork up` calls `BtrfsEngine::adopt_project` (`crates/slipfork/src/world/runtime.rs:67-75`, before any build). For a checkout on Btrfs that is not itself a subvolume, `convert_in_place` copies the tree to `<dir>.slipfork-adopt`, runs `fs::remove_dir_all(<dir>)`, then renames the copy in. Every shell/editor holding the old directory gets a deleted inode: `ls` is empty, `pwd` prints the logical path, `current_dir()` fails ENOENT, and a Dockerfile typo afterwards makes the whole run look like data loss. Owner hit exactly this (files were intact after stepping out and back, but the swap is unacceptable).

# Contract

- **`slipfork up` must never modify, rename, or replace the user's checkout.** No adoption, no sibling temp directories, no inode change.
- **`fork` on a plain (non-subvolume) checkout copies it** into `<project>/.forks/<fork>` with reflink-backed `fs::copy`, leaving the parent directory untouched. If the checkout *is* a subvolume, keep the existing `SNAP_CREATE_V2` fast path.
- Delete the adoption machinery entirely (pre-1.0 scorched earth).

# Pre-baked findings

- `crates/slipfork/src/world/runtime.rs:67-75`: remove `.adopt_project(project)`; keep `engine.ensure_store()`.
- `crates/slipfork-world/src/engine.rs:210-222` (`adopt_project`), `crates/slipfork-world/src/engine/convert.rs`: delete the function and module; remove `adopt_project` from the `WorldEngine` port (`port.rs`), `pub use`/`mod`, the `lib.rs` doc example, and every implementation/test double.
- `engine.rs:243-310` `fork`: the `Self::open_subvolume(project)?` probe at ~250 must accept a plain directory. Keep the project in `jobs` as a snapshot only when `btrfs::is_subvolume_path(project)`; otherwise create `project_snapshot` as a plain dir and reproduce the tree. Move `copy_tree` (from `convert.rs`) to a new `crates/slipfork-world/src/checkout.rs` as `pub fn copy_checkout(from, to)` (mode-preserving dirs, reflink `fs::copy` files, real symlinks, fail closed on nested mounts/FIFOs/sockets) and unit-test it under `/var/tmp`.
- `crates/slipfork/src/daemon/lifecycle/fork.rs:88-95`: remove the `not-btrfs` `is_subvolume_path` refusal; keep every other validation. Verify `drop`/teardown (`teardown::destroy_if_present`) removes a copied (non-subvolume) fork directory correctly.
- `crates/slipfork-world/src/bin/world_init/fault.rs:21`: drop the removed `slipfork daemon install` reference (M3.79 owns the rest of the packaging/docs wording).
- Tests: real Btrfs lab proof that **`up` leaves the checkout inode, contents and sibling list untouched** (record `st_ino` + `ls` before/after; no `.slipfork-adopt*`), and that `fork` on a plain checkout copies the files into `.forks/<name>` while the parent inode is unchanged. The CI `live-test` job has Btrfs.
- Do **not** touch `docs/**`, `packaging/**`, `crates/slipfork/src/cli/**`, `commands/**`, `daemon/client.rs` (M3.79 owns them).

# Acceptance

- `cargo fmt --all -- --check`, `clippy -D warnings`, `make check`; targeted `critical_fork_live` + `critical_world_journey`.
- Live on `165.232.86.174`: `mkdir /home/slipfork/plaincheck` (**plain dir on Btrfs, not a subvolume**), add files + a git repo; `slipfork up` there: inode unchanged, files unchanged, no adopt sibling, `cd`/`ls` in the original shell still work. Then `slipfork fork` shows the files under `.forks/<name>` and the parent is untouched.
- Commit `[M3.80] ...`; `M3.80 -> done`; no push.
