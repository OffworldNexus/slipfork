---
id: M2.2
title: "Workspace wedge: scaffold slipfork-edge/slipfork-vcs/slipfork-ospp and declare M2 dependencies"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/*/Cargo.toml"
  - "crates/slipfork-edge/**"
  - "crates/slipfork-vcs/**"
  - "crates/slipfork-ospp/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Create the three new M2 crates (`crates/slipfork-edge`, `crates/slipfork-vcs`, `crates/slipfork-ospp`) as empty, documented skeletons, and make this node the **single owner** of root `Cargo.toml` and `Cargo.lock` for the whole milestone, so no later M2 leaf ever has to touch the lockfile.

# Context

- Today `[workspace] members` lists only `slipfork-world`, `slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork`; `crates/slipfork-engine` has no `slipfork-world` path dependency yet (M1.6 follow-up #12) and `slipfork-world` has no hashing dependency.
- Every leaf that adds a dependency rewrites `Cargo.lock`'s per-package `dependencies` arrays. Doing that from ~15 parallel leaves would serialize them at `plan.py set active`; doing it once here keeps the whole graph parallel.
- `ADR-0008` (locked by `M2.1`) is the authority for which external crates and which crate boundaries exist; read it before editing manifests.

# Design / Invariants

1. **Skeletons only.** Each new crate ships `Cargo.toml`, `src/lib.rs` with `#![forbid(unsafe_code)]` plus `//!` docs naming the DI ports it will own (edge: sole ingress + `DaemonControl` + `WorldRegistry`; vcs: `VcsWorkspace`/`VcsWorkspaceFactory`/`RepoRootResolver`; ospp: `DeviceKeySigner` + OSPP v1 client + resource/dump ports), and empty module stubs. No behaviour, no `todo!()` graphs, no dead `pub` APIs beyond the documented stubs.
2. **Dependency wedge.** Add to `[workspace.dependencies]` exactly the external crates `ADR-0008` names (expected minimal set: a sync WebSocket client/server, an ECDSA P-256 implementation, `zstd`, and a socket-activation helper; reuse the already-locked `serde`, `serde_json`, `sha2`, `hex`, `thiserror`, `libc`, `toml`, `tempfile`, `clap`). Resolve versions with `cargo add --workspace` — never hand-write or invent a version.
3. **Crate-level edges.** Declare every dependency each crate will consume in this milestone exactly once here: `slipfork-world` (+`sha2`, `hex` for the service-label hash), `slipfork-engine` (+`slipfork-world` path dep, closing M1.6 #12), `slipfork-edge` (+`slipfork-world`, `slipfork-engine`, `serde`, `serde_json`, `thiserror`), `slipfork-vcs` (+`slipfork-world`, `serde`, `serde_json`, `thiserror`), `slipfork-ospp` (+`slipfork-world`, `slipfork-snow`, `slipfork-dag`, `serde`, `serde_json`, `sha2`, `hex`, `getrandom`, WebSocket + P-256 + `zstd` crates), and `crates/slipfork` (+`slipfork-edge`, `slipfork-vcs`, `slipfork-ospp` path deps, plus `slipfork-dag` with the `fs` feature that `M2.5` introduces).
4. **Dependency wedge, feature seam, no cruft.** Delete the lone `publish = false` in `crates/slipfork-dag/Cargo.toml` so the workspace is symmetric (M1.9 minor). The one exception to the "no empty feature tables" rule: `crates/slipfork-dag/Cargo.toml` declares

   ```toml
   [features]
   default = ["fs"]
   fs = []
   ```

   in this node, so `M2.2` compiles cleanly while `FsMerkleHasher`/`FsDigestCache` are still unconditional. `M2.5` then flips `default = []` and moves those two adapters behind `#[cfg(feature = "fs")]`, which is exactly what makes the `features = ["fs"]` line that `crates/slipfork/Cargo.toml` already carries meaningful. No other crate gets a `[features]` table. **H-009 Option B is out of scope here:** the `tier = "none"` `incus-engine` removal from `support/integrations.toml` lives in `M2.5`, the only leaf that owns `support/**`; this node never touches `support/**`.

5. **Unused-dependency discipline:** each declared dependency must be consumed by a named M2 leaf (record the mapping in the node log). The final milestone docs/cleanup gate re-checks that no declared dependency stayed unused.
6. `make check` must be green at the end of this node (skeletons compile, clippy clean, docs untouched).

# Acceptance

```bash
# 1. New members exist and resolve.
test -f crates/slipfork-edge/Cargo.toml -a -f crates/slipfork-vcs/Cargo.toml -a -f crates/slipfork-ospp/Cargo.toml
cargo metadata --no-deps --format-version 1 > /dev/null

# 2. Skeleton crates compile with zero warnings, no unsafe.
rg -q 'forbid\(unsafe_code\)' crates/slipfork-edge/src/lib.rs crates/slipfork-vcs/src/lib.rs crates/slipfork-ospp/src/lib.rs
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

# 3. Engine -> world edge (M1.6 #12) and the fs feature wiring exist.
rg -q 'slipfork-world' crates/slipfork-engine/Cargo.toml
rg -q 'features = \["fs"\]' crates/slipfork/Cargo.toml
! rg -q 'publish = false' crates/*/Cargo.toml

# 4. The dag feature seam exists so this node compiles before M2.5 gates the adapters.
rg -c "^fs = " crates/slipfork-dag/Cargo.toml
rg -c "^default" crates/slipfork-dag/Cargo.toml
cargo check -p slipfork-dag --all-targets

# 5. Plan + repo gates.
uv run --no-project tools/plan.py validate
make check
```

Every command must exit `0`. Note for reviewers: `cargo clippy` does not flag declared-but-unused dependencies, so the body's dependency→leaf mapping is the contract that later leaves consume them.
