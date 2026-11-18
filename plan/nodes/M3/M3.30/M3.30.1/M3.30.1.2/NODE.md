---
id: M3.30.1.2
title: "Purge support-matrix bureaucracy and collapse duplicated helper forks"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "support/**"
  - "tools/**"
  - "docs/support-matrix.md"
  - ARCHITECTURE.md
  - zensical.toml
  - "crates/slipfork-dag/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T22:20"
updated: "2026-11-18T22:40"
---

# Goal
Section 2 + Section 5 of `plan/nodes/M3/M3.30/M3.30.1/NODE.md` (read it): delete 0%-covered support bureaucracy and leave one canonical owner per duplicated helper.

# Context (anchors already verified)
## Support bureaucracy (delete files + wiring)
- `crates/slipfork-dag/src/support.rs`, `crates/slipfork-dag/tests/critical_support.rs`
- `support/integrations.toml`, `tools/check_support.py`, `docs/support-matrix.md`
- Drop `support-check` / `support-sync` / `rs-test` targets from `Makefile`; remove support-matrix from `zensical.toml` nav; remove `Commands::Support` (`crates/slipfork/src/cli.rs:213`) + dispatch/render (`crates/slipfork/src/main.rs:1355-1399`).
## Duplicated helper forks -> single canonical owner
- Merkle glob hashing x3: `crates/slipfork-dag/src/lib.rs:350`, `crates/slipfork-dag/src/fs.rs:30`, `crates/slipfork-engine/src/rootfs/input.rs:20`.
- Identical 16 KiB TCP relay loops: `crates/slipfork-edge/src/relay.rs:57` == `crates/slipfork-engine/src/bin/slipfork-world-init.rs:433`.
- ALTCHA x2: `crates/slipfork-ospp/src/pairing.rs:133` vs `crates/slipfork-ospp-testkit/src/reference_server.rs:723` (put the real verify/encode in the `slipfork-ospp` owner, testkit calls it).
- `quoted()` `crates/slipfork-dag/src/world.rs:223` vs `crates/slipfork-engine/src/rootfs/mod.rs:717`; `world_state_label()` `crates/slipfork/src/commands/status.rs:142` vs `crates/slipfork/src/commands/ps.rs:111`; `absolute_env()` `crates/slipfork/src/main.rs:209` vs `crates/slipfork/src/bin/git-remote-slipfork.rs:100`.
- Remove `static AtomicU64` at `crates/slipfork-engine/src/rootfs/mod.rs:725`.

# Constraints
- Canonical owner = the crate that owns the concept; no cross-crate leak of CLI tables into `slipfork-edge`. Scorched-earth, no aliases. No mocks/fakes. No RESULT.md.
- Deletion of `critical_support.rs` is the ONLY authorized protected-test deletion.

# Acceptance
- `! test -e crates/slipfork-dag/src/support.rs && ! test -e support/integrations.toml && ! test -e docs/support-matrix.md && ! test -e tools/check_support.py`
- `! grep -rn "support-matrix\|check_support" zensical.toml Makefile`
- `! grep -rn "Commands::Support" crates/slipfork/src/`
- `! grep -rn "AtomicU64" crates/slipfork-engine/src/rootfs/mod.rs`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-dag -p slipfork-engine -p slipfork-edge -p slipfork-ospp`
- `cargo test -p slipfork --lib`
