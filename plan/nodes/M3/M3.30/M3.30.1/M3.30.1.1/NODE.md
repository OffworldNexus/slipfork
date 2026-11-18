---
id: M3.30.1.1
title: "Canonical snow claimed-eval + delete dead provider/conformance/event code"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-snow/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T22:20"
updated: "2026-11-18T22:29"
---

# Goal
Section 1 + Section 3 of `plan/nodes/M3/M3.30/M3.30.1/NODE.md` (read it): make `slipfork-snow` the only snow evaluator and delete dead provider/conformance/event code.

# Context (anchors already verified)
- `crates/slipfork/src/daemon.rs` `render_claimed` (~4185) and `resolve_mode_local` (~4109): delete; add the partial/claimed evaluation entry point to `crates/slipfork-snow/src/faceless.rs` + `model.rs` so `${{ }}` parsing, secret taint and normalization stay exclusively in `slipfork-snow`, then call it from daemon.
- Delete `MemorySecretProvider` and `LayeredSecretProvider` (`crates/slipfork-snow/src/provider.rs`); rewrite any provider tests onto the real disk/file-vault provider with `tempfile` (disk, not RAM `/tmp`).
- Delete prod-compiled conformance modules: `crates/slipfork-engine/src/conformance.rs`, `crates/slipfork-engine/src/rootfs/conformance.rs`, `crates/slipfork-snow/src/conformance.rs`; move assertions worth keeping into the owning crate `tests/`.
- Delete unemitted `BootEvent::Secrets` producer + render arm (`crates/slipfork-edge/src/daemon.rs`, `crates/slipfork/src/main.rs` ~1476).

# Constraints
- True Constructor IoC: no globals, no OnceLock, no std::env::var in library crates. Scorched-earth: no aliases, no #[deprecated], no "kept for tests".
- No mocks/fakes anywhere. No RESULT.md.

# Acceptance
- `! grep -rn "render_claimed\|resolve_mode_local" crates/slipfork/src/daemon.rs`
- `! grep -rEn "MemorySecretProvider|LayeredSecretProvider" crates/`
- `! test -e crates/slipfork-engine/src/conformance.rs && ! test -e crates/slipfork-engine/src/rootfs/conformance.rs && ! test -e crates/slipfork-snow/src/conformance.rs`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-snow -p slipfork-engine -p slipfork-edge`
- `cargo test -p slipfork --lib`
