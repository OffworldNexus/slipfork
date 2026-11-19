---
id: M3.30.4.1
title: "Single-build Makefile, no nested cargo build, Btrfs live-suite gating, poc dag-plan cleanup"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - Makefile
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - docs/knowledge/LESSONS.md
  - "tools/**"
  - "poc/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T02:02"
updated: "2026-11-19T02:15"
---

# Goal
Sections 1 + 3 + 4 of `plan/nodes/M3/M3.30/M3.30.4/NODE.md` (read it).

# Work
- Delete the nested `cargo build` from `crates/slipfork/tests/support/mod.rs`; use `CARGO_BIN_EXE_*`.
- `Makefile`: `make check` compiles test targets once and runs them; no workspace re-link per target; keep `rs-test` but split live Btrfs suites into a separate `rs-test-live` target NOT part of `check`.
- Gate world-booting Btrfs suites: `check` runs pure/loopback suites fast; invoking a Btrfs suite directly without Btrfs still fails closed loudly (existing panic) — never silently skip.
- `poc/run_benchmark.py`: remove the deleted `slipfork dag plan` call and its dead output parsing.
- Record the documented `tools/do_vm.py test` path for the live suites.

# Constraints
No protected assertion weakened; zero mocks. No RESULT.md.

# Acceptance
- no nested cargo build in tests/support/mod.rs
- no `dag plan` in poc/
- `make -n check` shows one cargo test/build compile invocation per target set
- `make check` exits 0 on this non-Btrfs host
- `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D warnings`
