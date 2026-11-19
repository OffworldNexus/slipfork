---
id: M3.30.2.2.1
title: "Mechanical daemon.rs extraction into daemon/{mod,types,service,world,control,resolve,backing,env,tests}.rs"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T00:31"
updated: "2026-11-19T00:48"
---

# Goal
Slice A of `plan/nodes/M3/M3.30/M3.30.2/M3.30.2.2/NODE.md`: extract the 5,353-LOC `daemon.rs` into cohesive modules with ZERO behavior change.

# Work
- `daemon/{mod,types,service,world,control,resolve,backing,env}.rs` + inline `daemon/tests.rs`; delete `daemon.rs`.
- `control.rs` holds the 20 explicit `DaemonService::<verb>(self, …)` forwarders over inherent bodies so `impl DaemonControl` is no longer one 794-LOC block.
- Keep the inline `#[cfg(test)]` tests as `daemon/tests.rs` (they call private items; do NOT widen the API to move them to `tests/`).

# Constraints
Byte-identical diagnostics/`next:` text and control protocol; no behavior change; no aliases; cohesive structs `<= 500` LOC; every file `<= 560` LOC (use as many of the listed modules as needed). No RESULT.md.

# Acceptance
- `test ! -e crates/slipfork/src/daemon.rs`
- `for f in crates/slipfork/src/daemon/*.rs; do test $(wc -l < "$f") -le 560 || echo "TOO LONG $f"; done`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test daemon_unit_install`
