---
id: M2.15
title: "Benchmark and support-compliance tooling hardening (poc/run_benchmark.py, tools/check_support.py)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.17]
paths: ["poc/**", Makefile, tools/check_support.py, "tools/tests/**", "docs/support-matrix.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:43"
---

# Goal

Harden the two Python harness surfaces that the milestone gate depends on: `poc/run_benchmark.py` becomes non-contaminating and hermetic in its child environment, and the new `tools/check_support.py` becomes the single read-only compliance ceiling plus the only explicit support-registry mutator.

# Context

- `docs/research/m1-poc-benchmark.md` is a committed report. Today a plain `make bench` rewrites it, so any re-run dirties the working tree and hides real regressions.
- Outstanding benchmark follow-ups from `M1.7`: #1 child environment built from `os.environ`, #2 no fail-closed pre-run wipe of stale run state, #3 the report writer is reachable from the default target, #4 duplicated markdown/HTML row projection, #5 dead flags in the PoC entry point, #7 the seed plus `pg_restore` path is not exercised by a measured case, #16 the harness can time a non-empty stale directory.
- `ADR-0007` as amended by `M2.1` requires: `signoff_commit` proven read-only from `git`, `ci_job` proven to exist under `.github/workflows`, the released docs support table synced from `support/integrations.toml`, and `required_evidence` consumed from the single Rust ladder (`slipfork support --json`) instead of being re-derived in Python.
- `H-009` Option B is ratified: the registry only lists user-reachable integrations, and demotion back to a lower tier is an explicit, reasoned operation.

# Design / Invariants

1. **`bench_child_env` allowlist.** The benchmark child process receives exactly `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `LC_CTYPE`, plus the explicit `SLIPFORK_*` variables the run itself sets. No `os.environ` passthrough, no implicit locale or proxy inheritance; a missing allowlisted variable is omitted rather than replaced by a host default.
2. **Fail-closed pre-run wipe.** Before any timing, the run-state directory (default `~/.cache/slipfork/bench`, overridable by flag) is removed and re-created; a failed or partial removal exits non-zero **before** the first measurement, and a non-empty directory at measurement time is a hard error.
3. **Read-only by default.** `poc/run_benchmark.py --check` re-measures and compares against the committed report, printing a diff and exiting non-zero on drift, and never writes. `make bench` maps to `--check`. `make bench-update` (equivalently `--write-report`) is the only mutator, and the updated file must be reviewed and committed. Re-running `make bench` twice leaves `git status --porcelain` empty.
4. **One projection.** The measurement record renders the markdown table and the HTML table from the same structure; no second hand-maintained row list. Dead flags from #5 are deleted (not deprecated), and at least one measured case drives `initdb -> seed -> pg_restore`.
5. **`tools/check_support.py` (`ADR-0007` §3).** Read-only `--check` (wired into `Makefile`'s `check:` gate): invokes `cargo run -q -p slipfork -- support --json` (`M2.17`) so `required_evidence` comes strictly from the single Rust ladder, verifies `signoff_commit` exists and touches `signoff_doc` via read-only `git cat-file`/`git log`, verifies `ci_job` (`<workflow.yml>#<job_id>`) exists under `.github/workflows/`, verifies all present `recipe_page`/`signoff_doc`/`deploy_ref`/`production_suite_ref`/`test_audit` paths and `critical_test` symbols exist, and verifies `docs/support-matrix.md` is byte-identical to the rendered table from `support/integrations.toml` (drift exits `1`, never mutating files). `--json` prints the machine-readable verdict.
6. **One explicit mutator (`make support-sync` / `tools/check_support.py --sync`, `ADR-0007` §3).** Inspects every entry in `support/integrations.toml`: when required evidence for an entry's claimed tier is missing, prints each demotion (`id`, `from -> to`, missing pointer), lowers affected entries in `support/integrations.toml` to the highest tier whose required evidence remains present (never promoting; failing closed if no valid lower tier exists under `H-009` Option B), and writes `docs/support-matrix.md`. Stdlib only, executed with `uv run --no-project`.
7. **Tests.** `tools/tests/` covers the env allowlist, wipe failure, read-only default, automatic evidence-driven `--sync` demotion (never promotion), and `docs/support-matrix.md` drift detection; `make py-test` and `make py-lint` stay green.

# Acceptance

```bash
# 1. Harness unit tests.
make py-lint
make py-test

# 2. Read-only benchmark check leaves the tree clean and detects drift.
uv run --no-project poc/run_benchmark.py --check --help > /dev/null
git status --porcelain docs/research/m1-poc-benchmark.md | (! rg -q .)

# 3. Support compliance ceiling is read-only and total.
uv run --no-project tools/check_support.py --check
uv run --no-project tools/check_support.py --check --json > /dev/null
git diff --exit-code docs/support-matrix.md
make check
```

All commands exit `0`. Adversarial assertions required: `--sync` demotes an entry with missing evidence to its highest satisfied tier (and never promotes), a stale non-empty run directory aborts before timing, a deliberately drifted `docs/support-matrix.md` makes `--check` exit non-zero without mutating it, and an integration missing its `critical_test` or `signoff_commit` attestation is reported by id.
