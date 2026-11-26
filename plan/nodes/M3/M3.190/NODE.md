---
id: M3.190
title: "Law 1 finish: progress.rs/builder-mod splits, inline test extraction, dead code, terminal env"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/daemon/reconcile/**"
  - "crates/slipfork-image/src/builder/**"
  - "crates/slipfork-image/src/layout.rs"
  - "crates/slipfork-image/src/copy"
  - "crates/slipfork-image/src/copy.rs"
  - "crates/slipfork-image/src/registry"
  - "crates/slipfork-image/src/registry.rs"
  - crates/slipfork/src/ui/terminal.rs
  - "crates/slipfork/src/ui/report/**"
  - "crates/slipfork/src/world/runtime/**"
  - crates/slipfork/src/world/supervise
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/src/compose.rs
  - crates/slipfork/src/commands/docker/build
  - crates/slipfork/src/commands/docker/build.rs
  - "crates/slipfork/src/boot/build/tests/**"
  - "crates/slipfork-world/src/pod_env"
  - "crates/slipfork-world/src/pod_env.rs"
  - "crates/slipfork-world/src/boot/schedule"
  - "crates/slipfork-world/src/boot/schedule.rs"
  - "crates/slipfork-world/src/ledger/db.rs"
  - "crates/slipfork-world/src/ledger/db/**"
  - "crates/slipfork-forkfile/src/boot"
  - "crates/slipfork-forkfile/src/boot.rs"
  - "crates/slipfork-forkfile/src/migrate/snow.rs"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-image/tests/image_core_lab.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T23:40"
updated: "2026-11-26T02:43"
---

# Gate findings

1. **Prod files still >500 LOC:** `daemon/reconcile/progress.rs` (619 prod / 956 total) and `slipfork-image/src/builder/mod.rs` (524 prod / 577 total). Split them.
2. **Ten `src/` files exceed 500 *total* LOC** because of inline tests: `copy.rs` 1046, `world/runtime/helpers.rs` 834, `registry.rs` 654, `pod_env.rs` 653, `commands/docker/build.rs` 597, `forkfile/src/boot.rs` 571, `ui/report/stream.rs` 560, `world/supervise.rs` 555, `world/src/boot/schedule.rs` 547, `ui/report/render.rs` 521 (plus `src/boot/build/tests/{order,layout}.rs`). Move the `#[cfg(test)]` blocks to sibling `tests.rs` files so **every `.rs` file is ≤500 LOC unconditionally** — do not delete tests.
3. **Dead code:** delete `ImageLayout::staging` (`slipfork-image/src/layout.rs:40-45`, zero callers) and the stale `#[allow(dead_code)]` at `world/runtime/frames.rs:29`, `migrate/snow.rs:113,136`.
4. **Composition root:** `ui/terminal.rs:60-61` reads `NO_COLOR`/`TERM`; pass those values from `compose.rs` instead (library-crate env reads are forbidden).

# Acceptance

- `find crates -name '*.rs' -not -path '*/target/*'` → no file over 500 lines; `rg 'allow\(dead_code\)'` → none; `rg 'std::env' crates/slipfork/src/ui` → none.
- Behaviour byte-identical (PTY/JSON captures where touched); `make check`; Triad (arch + tests); commit `[M3.190] ...`, done, no push.
