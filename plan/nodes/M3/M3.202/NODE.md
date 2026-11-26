---
id: M3.202
title: "up fails ENOENT on the data-home store fallback (never created) + wrong remedy"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/src/engine.rs"
  - "crates/slipfork-world/src/log_store.rs"
  - "crates/slipfork/src/daemon/image/**"
  - crates/slipfork/src/world/mod.rs
  - "crates/slipfork-world/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-26T15:36"
updated: "2026-11-26T15:45"
---

# Bug (blocks `up` entirely on non-Btrfs / unwritable-mount projects)

```
$ slipfork up
error: cannot prepare the image store: could not open `/home/remy/.local/share/slipfork`: No such file or directory (os error 2)
next: check that this project's Btrfs store is writable and retry
```

Two defects:

1. **The data-home fallback store is never created.** `BtrfsEngine::ensure_store` (`crates/slipfork-world/src/engine.rs:224`) calls `open_dir(&self.store_anchor())` first; `store_anchor` (`:50`) returns `layout.btrfs_mount()` when the store root does not exist. For the data-home fallback path (`<data_home>/slipfork/store`, chosen when the project mount is `/` or not writable) that directory does not exist yet, so `File::open` fails `ENOENT` before any `create_dir_all` runs. The `<mount>/.slipfork-store` case masked this because the mount always exists.
2. **The remedy is wrong and the message hides the real cause.** `ImageLab::for_project` (`crates/slipfork/src/daemon/image/mod.rs:173`) says "check that this project's Btrfs store is writable" for every failure, including a missing directory and a non-Btrfs data home. It must name the resolved root and the actual cause.

# Fix

- Create the store root (parents included) **before** opening the anchor: `fs::create_dir_all(layout.store_root())` then validate Btrfs. Keep `require_btrfs` as the check, so an ext4/XFS data home fails with "not Btrfs" and a real remedy (mount a workspace image, link `install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder`).
- Distinguish and name: missing path (created), not Btrfs (remedy = workspace image), not writable (remedy = permissions).
- No behaviour change on a Btrfs project mount (still `<mount>/.slipfork-store`).

# Tests

- Lab: `XDG_DATA_HOME` pointed at a fresh temp dir + a project whose mount resolves to `/` (or an unwritable mount) → `up` creates `<data_home>/slipfork/store` and either proceeds or fails with the *not-Btrfs* message, never `ENOENT`.
- Error text pins the resolved path and one `next:`.
- `make check`.

# Acceptance

- The owner's repro no longer produces `ENOENT`; on a non-Btrfs data home it says so, with the workspace-image remedy.
- Commit `[M3.202] ...` on `release/0.1.1`, done, no push.
