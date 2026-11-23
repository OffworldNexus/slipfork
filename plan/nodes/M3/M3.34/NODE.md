---
id: M3.34
title: "Enforce TESTING_PLAN.md human gate, E2E-first tests & make coverage audit"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.33]
paths: [AGENTS.md, Makefile, "tools/**", ".opencode/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:12"
updated: "2026-11-21T00:43"
---

# Goal

Mirror `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0` commit `ae9dca8` (`MB.55`): wire `make coverage` (`tools/coverage_report.py`) and enforce the `TESTING_PLAN.md` human gate + story-bound E2E-first reviewer mandate across `AGENTS.md` and `.opencode/**`.

# Context & Reference

- Reference implementation: `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/tools/coverage_report.py` + `Makefile` `coverage` target + `git -C /home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0 show ae9dca8`.
- **Disk, Never `/tmp` `tmpfs`:** Write instrumented coverage artifacts to `$CARGO_TARGET_DIR/cov` or `target/cov` (defaulting to `~/.cache/slipfork-tmp/cov` or `target/cov`, never `/tmp` RAM `tmpfs` and never polluting `target/debug`), including `--bin slipfork` and `--bin slipfork-world-init` objects so spawned daemon/init code is counted accurately, printing per-file line % + region % + compressed uncovered line ranges + the `0.0%` `env_clear()` caveat.
- **Agentic rules (`AGENTS.md` + `.opencode/agents/*.md` + `.opencode/skills/{testing,dx-first,plan-system}/SKILL.md`):**
  1. New user-facing feature, resource, protocol, or workflow -> propose pure user-story `TESTING_PLAN.md` delta (zero file/crate/code references) -> `uv run --no-project tools/plan.py --agent <role> human add --kind compat-question --node <id> "Validate TESTING_PLAN.md update for <feature>"` + `set <id> blocked`; no code/tests proceed before human validation, and no slice silently edits `TESTING_PLAN.md`.
  2. Every test maps to a story in `TESTING_PLAN.md` and runs **end-to-end against the real binary** by default; non-E2E tests exist **strictly** for `TESTING_PLAN.md` §7 (`Forkfile` syntax/checker/fmt/migrate + doc highlighter, `slipfork-wire`/`slipfork-ospp` framing, `slipfork-lsp` + Zed).
  3. `review-tests` and `review-arch` reject story-less tests, unit tests duplicating E2E, and vacuous oracles (pass-when-skipped, exit-code/substring-only checks without real HTTP/DB/Btrfs/socket state), and run `make coverage TESTS="..." FILES="<changed crates/ files>"` to demand **dead-code deletion** or **E2E story test extension** (never filler unit tests).

# Acceptance

- `make coverage TESTS="-p slipfork-forkfile" FILES="crates/slipfork-forkfile/src/fmt.rs"` runs cleanly into `cov` target dir and prints line %, region %, and compressed uncovered line ranges.
- `AGENTS.md` and `.opencode/**` enforce the `TESTING_PLAN.md` human gate, E2E-first rule, and `make coverage` reviewer mandate cleanly while preserving all 5 Engineering Laws.
- `uv run --no-project tools/guard.py` and `uv run --no-project --with pytest pytest -q tools/tests` exit `0`.
