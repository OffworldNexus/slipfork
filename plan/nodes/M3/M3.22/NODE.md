---
id: M3.22
title: "Delete leftover slipfork-ospp test-server Cargo feature; move reference OSPP server doubles into tests/support"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.1, M3.15]
paths:
  - "crates/slipfork-ospp/Cargo.toml"
  - "crates/slipfork-ospp/src/lib.rs"
  - crates/slipfork/Cargo.toml
  - crates/slipfork/tests/critical_ospp_daemon.rs
  - crates/slipfork/tests/support/mod.rs
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T18:15"
updated: "2026-11-16T19:16"
---

# Goal

Delete the leftover M2 `test-server` Cargo feature from `crates/slipfork-ospp` and replace it with pure test-side doubles, so no production crate publishes a test-scaffolding feature and the reference OSPP server never links into the shipped `slipfork` binary.

# Context

- `crates/slipfork-ospp/Cargo.toml:41` declares `test-server = ["dep:tempfile", "dep:tpmd-proto"]`, `crates/slipfork-ospp/src/lib.rs:45-51` gates `pub mod test_server;` / `pub mod test_server_tpmd;` behind it, and `crates/slipfork/Cargo.toml:29` enables it from a **production** `[dependencies]` entry (`slipfork-ospp = { path = ..., features = ["test-server"] }`), so the reference server and `tempfile`/`tpmd-proto` ship in the release binary purely to serve `crates/slipfork/tests/critical_ospp_daemon.rs`.
- OWNER DIRECTIVE (2026-11-16): zero test-scaffolding Cargo features anywhere; features are reserved strictly for language/platform modularity. Test doubles live in `#[cfg(test)]`, `tests/common/`/`tests/support/`, or an unpublished `publish = false` testkit crate consumed via `[dev-dependencies]`.

# Design / Invariants

1. Move the reference `ospp-v1` server and `FakeTpmd` test doubles out of `crates/slipfork-ospp/src/` into `crates/slipfork/tests/support/` (test-binary-only module tree), rewriting `crates/slipfork/tests/critical_ospp_daemon.rs` to import them from `support::` exactly as it imports the other harness helpers.
2. Delete the `test-server` feature, its comment, and the `[features]` table from `crates/slipfork-ospp/Cargo.toml`; drop the `features = ["test-server"]` selector from `crates/slipfork/Cargo.toml` (production dependency stays plain). Remove now-unused `tempfile`/`tpmd-proto` optional wiring from `slipfork-ospp` and keep them only where the moved doubles compile (dev-dependencies of `slipfork`).
3. No behaviour change: the OSPP v1 handshake, fragment pairing, and reference-server coverage in `critical_ospp_daemon.rs` must keep passing with every assertion intact (the file is a protected critical test; this node deliberately authorizes editing it).
4. Verify the shipped binary is clean: `cargo tree -p slipfork --edges normal` must show NO `tpmd-proto`/`tempfile` pulled in for the reference server, and `grep -rn 'test-server\|test_server' crates/ ` must only match test code.
5. **Determinism fallout from `M3.1` attempt 3:** the same file's `s3_buckets` scenario flaked once at a fixed 60s readiness wait (`M3.1` log, "transient `critical_ospp_daemon` `s3_buckets` 60s readiness flake"). Replace the one-shot readiness wait with the existing bounded `poll_until`-style pattern (`tests/support/`), keeping the deadline and surfacing captured diagnostics on timeout. No assertion weakening, no `sleep()` padding.

# Acceptance

```bash
! grep -rn 'test-server' crates/ Cargo.toml
! grep -rn 'features = \["test' crates/*/Cargo.toml
cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
make check
```

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
