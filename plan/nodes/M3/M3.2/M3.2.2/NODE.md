---
id: M3.2.2
title: "Cargo wedge: crates/slipfork-test crate skeleton, workspace member, full dependency set"
kind: task
status: superseded
owner: implementer
priority: 1
depends_on: [M3.2.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-test/Cargo.toml"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/src/error.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-18T22:19"
---

# Goal

Land the serialized Cargo wedge: create `crates/slipfork-test` (`#![forbid(unsafe_code)]`) as a workspace member with its full dependency set, module skeleton, and error type, so every later Phase 2 leaf only edits `src/**` and never touches `Cargo.toml`/`Cargo.lock` again.

# Context

- The path lock is per-file: root `Cargo.toml` and `Cargo.lock` are the only true serialization points of this epic, so they must be written **once**, up front, with every dependency the crate will ever need (`ADR-0010` engine types come from `slipfork-engine`; DAG types from `slipfork-dag`; `DaemonControl`/`TestEvent` types from `slipfork-edge`; pure identity from `slipfork-world`).
- `M3.2.1` locked the trait/seam names; this node materialises the crate that will hold them.

# Design / Invariants

1. `crates/slipfork-test/Cargo.toml`: `#![forbid(unsafe_code)]` crate, `version.workspace = true`, dependencies `slipfork-world`, `slipfork-dag`, `slipfork-edge`, `slipfork-engine` (path deps), `serde`/`serde_json`, `thiserror`, `hex`, `sha2`, `libc` only if a leaf provably needs it; dev-deps `tempfile`. Nothing speculative beyond the locked blueprint.
2. Root `Cargo.toml`: add `"crates/slipfork-test"` to `[workspace] members`; no version bump here (`M3.2.17` owns `0.3.0`).
3. `src/lib.rs`: `#![forbid(unsafe_code)]`, crate docs naming the canonical run tree, `pub mod` declarations only for modules that exist in *this* node (start with `error`), and `pub use` of the crate error type. No empty placeholder modules, no `todo!()`.
4. `src/error.rs`: `TestError` with typed variants the blueprint earns (`Protocol`, `CoverageUnsupported`, `InvalidCoverageThreshold`, `SlotBoot`, `Engine`, `Fixture`, `Io`), each with a precise message; no catch-all `String` variant beyond the underlying `Io`/`Engine` sources.
5. `cargo check -p slipfork-test` and `cargo clippy -p slipfork-test --all-targets -- -D warnings` must pass; `Cargo.lock` updated by cargo, never hand-edited.

# Acceptance

```bash
cargo check -p slipfork-test
cargo clippy -p slipfork-test --all-targets -- -D warnings
cargo test -p slipfork-test --all-targets
grep -n 'forbid(unsafe_code)' crates/slipfork-test/src/lib.rs
grep -n 'crates/slipfork-test' Cargo.toml
grep -c 'name = "slipfork-test"' Cargo.lock
! grep -rn 'todo!\|unimplemented!\|placeholder' crates/slipfork-test/src/
make rs-test
```

Expected: check/clippy/tests exit 0; `lib.rs` carries the `forbid` attribute; the workspace lists the crate and `Cargo.lock` contains exactly one `slipfork-test` package entry (count `1`); the inverted `grep` prints nothing; `make rs-test` stays green.
