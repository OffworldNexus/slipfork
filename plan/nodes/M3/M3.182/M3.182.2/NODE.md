---
id: M3.182.2
title: Split the CLI world supervise/runtime/up modules and delete dead hostname
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.182.1]
paths: ["crates/slipfork/src/world/**", crates/slipfork/src/compose.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T21:45"
updated: "2026-11-25T22:01"
---

# Goal

Split the three over-budget CLI world modules into cohesive directory modules; delete the dead `hostname` field.

# Context (pre-baked, verified at HEAD 4173bd7)

- `crates/slipfork/src/world/supervise.rs` = 846 lines: `impl Session` 27, `supervisor_main` 59-294, `stamps_for` 295, `boot_event_json` 303, `step_index` 401, `Emitter` 412, `BootSink` 437/446, `EventLog` 524/533, tests 602. Target `world/supervise/{mod,events,sink,log}.rs`; `world/mod.rs` keeps `mod supervise;` and existing re-export paths unchanged.
- `crates/slipfork/src/world/runtime.rs` = 566 lines with existing `runtime/{dev,execute,helpers,ready,resources,rootfs}.rs`: move the `impl WorldRuntime` body 80-563 in cohesive method groups plus `elapsed_ms` 564 into a seam module (e.g. `runtime/lifecycle.rs`); no parameter bags, no new traits.
- `crates/slipfork/src/world/up.rs` = 543 lines: `impl Session` 22-501, `ps_reply` 502, `boot_node` 516 -> `world/up/{mod,reply}.rs`.
- `crates/slipfork/src/world/mod.rs:69-72`: delete `pub hostname: String` + its `#[allow(dead_code)]` and its single assignment in the composition root (`grep -rn "hostname" crates/slipfork/src`).

# Acceptance

- Production LOC <= 500 per file (`awk '/^#\[cfg\(test\)\]/{f=1} !f' | wc -l`).
- `grep -rn "hostname" crates/slipfork/src` has no dead reader left behind.
- Existing PTY/byte goldens for `up`/`down`/`status` unchanged; `cargo test -p slipfork` green; `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Do **not** touch `crates/slipfork/src/daemon/**`, `ui/**`, `boot*`, `commands/**`, `doctor/**`, `status/**` (other slices), `specs/`, `TESTING_PLAN.md`, `CHANGELOG.md`, `docs/`, or any `critical_*.rs`.
