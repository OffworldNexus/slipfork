---
id: M3.22
title: "Delete leftover slipfork-ospp test-server Cargo feature; move reference OSPP server doubles into tests/support"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.1, M3.15]
paths:
  - ARCHITECTURE.md
  - "crates/slipfork-ospp/Cargo.toml"
  - "crates/slipfork-ospp/src/lib.rs"
  - "crates/slipfork-ospp/tests/**"
  - "crates/slipfork-ospp-testkit/**"
  - crates/slipfork/Cargo.toml
  - crates/slipfork/tests/support/mod.rs
  - crates/slipfork/tests/critical_ospp_daemon.rs
  - Cargo.toml
  - Cargo.lock
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T18:15"
updated: "2026-11-17T03:18"
---

# Goal

Delete the leftover M2 `test-server` Cargo feature from `crates/slipfork-ospp` and replace it with pure test-side doubles, so no production crate publishes a test-scaffolding feature and the reference OSPP server never links into the shipped `slipfork` binary.

# Context

- `crates/slipfork-ospp/Cargo.toml:41` declared `test-server = ["dep:tempfile", "dep:tpmd-proto"]`, `crates/slipfork-ospp/src/lib.rs:45-51` gated `pub mod test_server;` / `pub mod test_server_tpmd;` behind it with `#[path]` includes into `crates/slipfork-ospp/tests/support/`, and `crates/slipfork/Cargo.toml` enabled it from a **`[dev-dependencies]`** entry (`slipfork-ospp = { path = ..., features = ["test-server"] }`; corrected by the legality gate — the shipped normal tree never carried `tempfile`). A production crate still published a test-scaffolding feature and its test files were compiled through a cross-package `#[path]`; this node deletes both and moves the doubles into the `publish = false` `slipfork-ospp-testkit` crate.
- OWNER DIRECTIVE (2026-11-16): zero test-scaffolding Cargo features anywhere; features are reserved strictly for language/platform modularity. Test doubles live in `#[cfg(test)]`, `tests/common/`/`tests/support/`, or an unpublished `publish = false` testkit crate consumed via `[dev-dependencies]`.

# Design / Invariants

1. Move the reference `ospp-v1` server and `FakeTpmd` test doubles out of `crates/slipfork-ospp/tests/support/` into the new `publish = false` workspace crate `crates/slipfork-ospp-testkit` (one canonical owner, consumed through `[dev-dependencies]` by both `crates/slipfork-ospp` and `crates/slipfork`; no cross-package `#[path]` include), rewriting `crates/slipfork/tests/critical_ospp_daemon.rs` and `crates/slipfork-ospp/tests/critical_ospp.rs` to import them from `slipfork_ospp_testkit::…`.
2. Delete the `test-server` feature, its comment, the `[features]` table, and the cross-package `#[path]` modules from `crates/slipfork-ospp`; drop the `features = ["test-server"]` selector from `crates/slipfork/Cargo.toml` (the dependency stays plain). Remove now-unused `tempfile`/`tpmd-proto` optional wiring from `slipfork-ospp` and let the testkit crate carry the doubles' real dependency set.
3. No behaviour change: the OSPP v1 handshake, fragment pairing, and reference-server coverage in `critical_ospp_daemon.rs` must keep passing with every assertion intact (the file is a protected critical test; this node deliberately authorizes editing it).
4. Verify the shipped binary is clean: `cargo tree -p slipfork --edges normal` must show **no new direct edge** to `tempfile`/`tpmd-proto`/`tpmd-ipc`/`tungstenite`/`zstd`/`p256` (`tpmd-proto` legitimately remains through the pre-existing `tpmd-client` chain), and `grep -rn 'test-server\|test_server' crates/` matches nothing.
5. **Determinism fallout from `M3.1` attempt 3:** the same file's `s3_buckets` scenario flaked once at a fixed 60s readiness wait (`M3.1` log, "transient `critical_ospp_daemon` `s3_buckets` 60s readiness flake"). Replace the one-shot readiness wait with the existing bounded `poll_until`-style pattern (`tests/support/`), keeping the deadline and surfacing captured diagnostics on timeout. No assertion weakening, no `sleep()` padding.

> **Lead note (assertion baseline).** Unchanged before and after: `crates/slipfork/tests/critical_ospp_daemon.rs` has 4 tests / 36 assertions, `crates/slipfork-ospp/tests/critical_ospp.rs` has 32 tests / 170 assertions. Score the protected-test check against those numbers.

# Acceptance

```bash
! grep -rn 'test-server' crates/ Cargo.toml
! grep -rn 'features = \["test' crates/*/Cargo.toml
cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
make check
```

## Load envelope (owner ruling 2026-11-16, binding)

`make check` must be green at load average ≤ 10 with zero in-repo builds. Until `M3.21` deletes the pre-`Btrfs` engines, their fixed 10 s `WORLD_STOP_GRACE` and fixed readiness budgets red-line world-booting suites under unrelated foreign load (≈ 40+). When that is the only red, record it verbatim as the deleted engine's residual (owned by `M3.13`/`M3.14`/`M3.21`) and verify the deterministic gate set instead: `pytest -q tools/tests`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `make docs-check`, `guard.py`, `critical_guard.py`, `plan.py validate`, plus the suites this node owns. Never retry-until-green and never hide a red. `cargo tree -p slipfork --edges normal` must be clean of `tpmd-proto`/`tempfile`.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
