---
id: M3.45.1
title: "make coverage sweep, dead-code deletion & <=500 LOC / clippy clean"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/**", Makefile, "tools/**", TESTING_PLAN.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T22:40"
updated: "2026-11-21T22:52"
---

# Goal
Run the workspace coverage sweep and delete every leftover path from the old hand-rolled CLI/TUI and SeaweedFS implementations, so the 0.1.0 tree carries only code the shipped binary reaches. Prove `<= 500` LOC per source file and a warning-free workspace.

# Context
- Coverage entry point: `make coverage` (Makefile:36-37) -> `tools/coverage_report.py`, with `COVERAGE_FILES` covering every `crates/*/src/**/*.rs` (Makefile:34) and profiles/report landing in `target/cov`. Rows whose proving story is Btrfs/crun/OSPP live are out of scope here: they are proven by sibling slice M3.45.2 (`LIVE_SUITES`, Makefile:8-15). Never fake them with test-only code.
- Delete, do not test: any 0%-covered fallback/dead branch left by the superseded hand-rolled CLI/SeaweedFS work. Inspect especially `crates/slipfork/src/commands/mod.rs` (486 LOC, closest to the cap), `crates/slipfork/src/compose.rs`, `crates/slipfork/src/ui/**`, and the native S3 owner modules `crates/slipfork-world/src/resources/s3/policy.rs`, `object_ops.rs`, `listing.rs`. If a branch is a real error path the binary can hit, keep it and cover it with a real lab test; if nothing reaches it, delete it atomically.
- Canonical ownership: S3 policy/verb behaviour stays in `resources/s3/*`; CLI wiring stays in `crates/slipfork/src/commands/*`. Never re-scan headers or argv downstream, and never introduce a single-impl trait just to make a dead branch reachable.
- Honesty: zero `RESULT.md`; paste real tool output in your final message. Never raise the number by adding `Mock*`/`Fake*`/`Stub*` or `#[cfg(test)]`-only shims, and never weaken a `crates/*/tests/critical_*.rs` file (`tools/critical_guard.py` protects them).
- Host: local workstation only; no VM needed for this slice.

# Acceptance
- `make coverage` exits 0; quote the reported line-coverage number and list the uncovered files whose dead lines you deleted.
- `find crates -path '*/src/*' -name '*.rs' -exec wc -l {} + | sort -rn | head` shows no file above 500.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `cargo test --workspace` exits 0; `uv run --no-project tools/critical_guard.py` exits 0.
- `uv run --no-project tools/guard.py` exits 0.
- `git diff -U0 | grep -E '^\+.*(Mock|Fake|Stub|unimplemented!|todo!)'` returns nothing.
- If a deleted path was documented in `TESTING_PLAN.md`, that row is updated in the same change.
