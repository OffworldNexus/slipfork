---
id: M3.42.2
title: "make coverage target, dead-code sweep, comment diet & >500 LOC splits"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.42.1]
paths: ["crates/**", Makefile, "tools/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T17:30"
updated: "2026-11-21T17:30"
---

# Goal

Add a real `make coverage` target, then use it to delete 0%-covered dead code, strip redundant
comments, and split every `crates/*/src/**/*.rs` file over 500 LOC into cohesive modules without
introducing mock traits or new free-function galaxies.

# Context

- `Makefile` has **no** `coverage` target today (`grep -n 'coverage' Makefile` matches nothing).
  Targets that exist: `check:` (line 28), `guard:` (47), `test-tools:` (50), `docs-check:` (55),
  `live-test:` (102), `packages-probe:` (133), `packages-bundle:` (142), `packages:` (148),
  `packages-test:` (164).
- `tools/coverage_report.py` usage (verified from its docstring/CLI): requires `--tests "<cargo test args>"`
  and `--files "<globs>"`, builds with `-C instrument-coverage` into `target/cov`
  (`TARGET_DIR = Path("target/cov")`, never `/tmp`), merges profiles with `llvm-profdata`, and prints
  per-file line/region percentages plus uncovered ranges. Uncovered lines are never an error; exit
  status fails only on tooling/build/test failure.
- Wire `coverage:` to a workspace-breadth `--tests` (per-crate `-p <crate> --tests` set) and
  `--files` over `crates/*/src`; keep `COVERAGE_TESTS` / `COVERAGE_FILES` overridable variables so a
  bare `make coverage` works with sensible defaults.
- Verified LOC offenders (`wc -l`, all `> 500`): `crates/slipfork/src/commands/host.rs` 615,
  `crates/slipfork-image/src/registry.rs` 561, `crates/slipfork/src/install.rs` 514,
  `crates/slipfork-world/src/pod.rs` 505. Split along canonical ownership (§03 Crate & Port Map):
  `commands/host.rs` -> verb modules beside it, `install.rs` -> install steps, `registry.rs` -> OCI
  reference/auth vs manifest fetch, `pod.rs` -> pod spec vs `crun` invocation.
- Keep only comments carrying non-obvious host/kernel facts (Btrfs ioctls, `PR_SET_CHILD_SUBREAPER`,
  `-pgid`, AppArmor); delete narration comments and commented-out code.

# Acceptance

- `grep -n '^coverage:' Makefile` matches and `grep -n 'target/cov' Makefile` matches.
- `make coverage` exits 0 and leaves profiles/reports under `target/cov`.
- `find crates -path '*/src/*' -name '*.rs' -exec wc -l {} + | awk '$1 > 500 && $2 != "total"'` prints nothing.
- `cargo fmt --all -- --check` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `cargo test --workspace` exits 0.
