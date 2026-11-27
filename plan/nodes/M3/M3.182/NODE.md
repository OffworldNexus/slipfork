---
id: M3.182
title: "Law 1 & report-doorway cleanup: <=500 LOC world/daemon splits, typed Palette roles, zero globals/prints"
kind: epic
status: done
priority: 3
depends_on: [M3.180]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T19:22"
updated: "2026-11-25T22:45"
---

# Goal

Restore strict compliance with Law 1 (`<= 500` non-test LOC, zero non-root globals/prints, zero pre-`1.0` dead code) and close the remaining raw-SGR / stdout doorways in `crates/slipfork`.

# Evidence

1. **8 files exceed `<= 500` production LOC:** `crates/slipfork/src/daemon/lifecycle/fork.rs` (715 prod), `crates/slipfork-world/src/pod_log.rs` (645), `crates/slipfork/src/world/supervise.rs` (600), `crates/slipfork/src/world/runtime.rs` (566), `crates/slipfork/src/daemon/client.rs` (550), `crates/slipfork/src/world/up.rs` (543), `crates/slipfork/src/daemon/mod.rs` (523), `crates/slipfork-image/src/puller.rs` (511).
2. **Open `Palette::tint` & `boot.rs` raw prints:** `crates/slipfork/src/ui/report/palette.rs:47` keeps `pub fn tint` with raw SGR literals at `boot/render.rs:119,288,289,326,339,352,353,365,366`, `boot/phase.rs:127`, `doctor/mod.rs:81,84`, `status/tree.rs:306`, `commands/features.rs:171,181`, `commands/fmtmigrate.rs:242`; `boot.rs:159,202,221,360` still calls `println!`/`eprintln!` via `tools/guard.py:60`.
3. **Global/library leaks & dead residue:** `crates/slipfork/src/commands/pty.rs:18` holds `static RESIZED: AtomicBool`; `crates/slipfork-world/src/supervisor/{mod.rs:252,egress.rs:241}` call `eprintln!` in a library crate; `crates/slipfork/src/world/mod.rs:69` (`hostname` for deleted `commands/auth.rs`) and `crates/slipfork/src/boot/render.rs:241` carry dead `#[allow(dead_code)]`.

# Acceptance

- Every `.rs` source file in `crates/*/src/**` has `<= 500` production LOC.
- `Palette::tint` is narrowed to `pub(super)` behind named role methods, `boot.rs` removed from `tools/guard.py` `RAW_PRINT_ALLOWLIST`, `pty.rs` uses a self-pipe/injected signal state instead of `static AtomicBool`, `slipfork-world` library `src/` has zero `eprintln!`, and dead `#[allow(dead_code)]` items are deleted.
- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` and `tools/guard.py` exit `0`.
