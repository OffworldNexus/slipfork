---
id: M3.40.11
title: "slipfork-vcs GitWorkspace VcsWorkspace port + daemon BootSession consolidation"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.10]
paths:
  - "crates/slipfork-vcs/**"
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - "specs/03-under-the-hood.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T14:35"
updated: "2026-11-21T14:35"
---

# Goal
Move branch topology and dirty-snapshot detection behind the canonical `specs/03` `VcsWorkspace` port (`crates/slipfork-vcs`) and consolidate the daemon's fork boot into one `BootSession` value, closing the two architectural debts left by `M3.40.8`.

# Context
- `crates/slipfork-vcs/src/lib.rs` is a 13-line stub; `crates/slipfork` does not depend on it. `specs/03` §5 assigns `VcsWorkspace` → `GitWorkspace`/`JjWorkspace` to this crate.
- `crates/slipfork/src/daemon/lifecycle/fork.rs` currently hand-rolls `git rev-parse --abbrev-ref`, `git switch [-c]`, and `git status --porcelain` as three private free functions, and builds fork paths with a private `FORKS_DIR` constant instead of `slipfork_world::layout`/`StoreLayout`.
- `crates/slipfork/src/daemon/boot.rs` `finish_boot` is a 7-argument free function with a mutable `fork` out-param, and the `Mutex<Core>` guard is split between `fork_snapshot` and `begin_up`.

# Acceptance
- `slipfork-vcs`: `VcsWorkspace` port with `branch(&Path) -> Result<String>`, `switch(&Path, &str) -> Result<()>`, `dirtiness(&Path) -> Result<Dirtiness>` (or a crate-local `Dirty` value), plus `GitWorkspace` running real `git` through an injected executable path; unit/integration test against a real temporary git repo (`/var/tmp`, real commits, real dirty file).
- `slipfork`: depends on `slipfork-vcs`, injects one `GitWorkspace` into `Core` alongside `HostEnv`, and deletes the three private git free functions; fork paths/world names come from `slipfork_world` (`StoreLayout::project_fork`, `fork_world_name` or equivalent) with no local `FORKS_DIR`/`format!`.
- `finish_boot` becomes `BootSession { key, log, child, project, fork }` with `run(&mut self, emit)`, and the fork reservation happens under the same `Core` guard as `begin_up`.
- `cargo clippy -p slipfork-vcs -p slipfork --all-targets -- -D warnings`; `cargo test -p slipfork-vcs -p slipfork`; `critical_guard.py`; `guard.py`.
