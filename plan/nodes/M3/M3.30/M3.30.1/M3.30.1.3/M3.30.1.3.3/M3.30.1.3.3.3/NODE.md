---
id: M3.30.1.3.3.3
title: "Concrete browser open: delete BrowserOpener trait and NoBrowser across ospp/daemon/commands"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.3.3.2]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork-ospp/**", "crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T23:13"
updated: "2026-11-18T23:13"
---

# Goal
Split 3/4 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/M3.30.1.3.3/NODE.md`: browser opening is one concrete struct.

# Work
- Delete `slipfork_ospp::pairing::BrowserOpener` and `NoBrowser`; add one concrete `Browser` whose constructor takes the resolved absolute opener program (production: `xdg-open`/`gio` resolved in the composition root); `open(url)` spawns that real binary.
- Update the 3 `DaemonService` constructors (`DaemonWiring`, `join_worlds`, daemon service) + both roots (`main.rs`, `bin/slipforkd.rs`) and `open.rs` docs.
- Tests pass a real executable script they create on disk (records argv to a file) — a real subprocess, not a mock/stub.

# Constraints
Composition-root-only program resolution; no traits; no aliases; no mocks. No RESULT.md.

# Acceptance
- `! grep -rEn "NoBrowser|trait BrowserOpener|BrowserOpener" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-ospp`
- `cargo test -p slipfork --lib`
