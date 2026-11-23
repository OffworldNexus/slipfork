---
id: M3.35
title: "Execute TESTING_PLAN.md test & codebase cleanup: E2E consolidation, GHA multi-platform stories & dead-code removal"
kind: epic
status: superseded
priority: 3
depends_on: [M3.34]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - AGENTS.md
  - ARCHITECTURE.md
  - README.md
  - CHANGELOG.md
  - "crates/**"
  - "packages/**"
  - "editors/**"
  - "support/**"
  - "poc/**"
  - "tools/**"
  - "docs/**"
  - ".github/**"
  - ".opencode/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:12"
updated: "2026-11-21T00:43"
---

# Goal

Execute the scorched-earth test and codebase cleanup aligned strictly to `TESTING_PLAN.md` (do **NOT** alter `TESTING_PLAN.md`), modeled directly on `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0` epic `MB.56`:
1. Write transient migration blueprint `/tmp/opencode/testing-migration-plan.md` (Keep/Merge/Delete inventory + measured prod/test LOC baseline & cut targets — **never committed to git**).
2. Delete retired/unused `0.1.0` crates, dead modules, and meta-linters (including unused `crates/slipfork-dag` if unreferenced by shipped `0.1.0` binaries — `0.2.0` builds `STP v1` fresh — plus `tools/critical_guard.py`, `tools/check_stub_adapters.py`, redundant meta-linter scripts & tests; update `Makefile`, `AGENTS.md`, `ARCHITECTURE.md`, and `.opencode/**`).
3. Delete all non-§7 inline `#[cfg(test)]` & micro-unit tests across `crates/*`, keeping **only** tight focused suites for `TESTING_PLAN.md` §7 (`slipfork-forkfile` syntax/checker/fmt/migrate + runnable doc fences, `slipfork-wire` & `slipfork-ospp` framing/HPKE, `slipfork-lsp`, `editors/zed`, `forkfile-highlight.test.js`).
4. Consolidate `crates/*/tests/*` and `packages/opencode-slipfork/tests/*` into crisp, zero-mock user-story E2E suites (`e2e_cli.rs`, `e2e_world_pods.rs`, `e2e_fork_agents.rs`, `e2e_ospp_edge.rs`, etc.) with zero duplicate world boots, and upgrade `.github/workflows/*` so multi-platform Linux (`x86_64` & `aarch64`), `install.sh` checksum refusal, Zed/LSP, docs, and real polyglot (`Python` + `Node/static` + `PHP` + `PostgreSQL 18` + `Redis 7` + `S3`) user stories in `TESTING_PLAN.md` §2–§6 are verified end-to-end.
5. Run `make coverage` locally and on live Btrfs droplet `165.232.86.174` across the consolidated E2E + §7 suites to delete uncovered dead Rust production code and strip historical diary/pivot comments across `crates/*`.

# Acceptance

- `TESTING_PLAN.md` unchanged; every story in §2–§7 protected by real E2E or §7 focused tests with zero non-§7 unit tests and zero vacuous oracles.
- Dramatic net reduction in test & dead production LOC recorded in `LOG.md`.
- Local `make check` + `make docs-check` exit `0`, AND live `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test` on `165.232.86.174` exit `0` with `/home/slipfork/demo` left running on `:80`.
