---
id: M3.35.5
title: "Coverage-driven dead-code deletion, comment stripping & live 165.232.86.174 gate"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.35.4]
paths: ["crates/**", Makefile, ARCHITECTURE.md, CHANGELOG.md, "tools/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:14"
updated: "2026-11-21T00:43"
---

# Goal

Close the epic: use `make coverage` (wired in M3.34 via `tools/coverage_report.py`) to delete uncovered Rust production code and historical diary comments, then prove the whole gate green locally AND on the live Btrfs droplet `165.232.86.174` with `demo` serving `:80`.

# Context

- `make coverage` arrives from M3.34 (`tools/coverage_report.py`, artifacts under `$CARGO_TARGET_DIR/cov` = `~/.cache/slipfork-tmp/cov`, never `/tmp` tmpfs, with `--bin slipfork` + `--bin slipfork-world-init`). Run it over the M3.35.3 E2E set + M3.35.2 §7 suites: `make coverage TESTS="-p slipfork -p slipfork-engine -p slipfork-edge -p slipfork-world -p slipfork-vcs --tests" FILES="$(git diff --name-only origin/develop...HEAD -- 'crates/**/*.rs' | tr '\n' ' ')"`.
- Delete any `0.0%`-coverage module/function/error-variant/helper left over from `0.1.0` (candidates to inspect: `crates/slipfork-dag/src/fs.rs` residue, `crates/slipfork/src/{hook_failure,apparmor,layout}.rs` dead arms, unreachable `LayeredSecretProvider`-style fallbacks, unused `thiserror` variants). If an uncovered branch maps to a real `TESTING_PLAN.md` §2–§6 story, ADD the assertion to the owning `e2e_*.rs` — never add a unit test.
- §7 keepers must not be deleted for low coverage (`slipfork-forkfile`, `slipfork-wire`, `slipfork-ospp`, `slipfork-lsp`, `editors/**`, `forkfile-highlight.test.js`): those are explicitly sanctioned focused suites.
- Strip multi-paragraph historical/pivot diary comments across `crates/**/src/**/*.rs` (`//! 0.1.0: ...`, `// Why we used to ...`, "Option A vs Option B" archaeology, plan-node `M3.x` essays). Keep 1–3 line Rustdoc on public items and every `// SAFETY:` on `unsafe`. Do NOT touch `docs/**`, `TESTING_PLAN.md`, or `plan/**`.
- `ARCHITECTURE.md` must match the post-cleanup tree (no `critical_guard.py`, no deleted DAG surface, no removed test-suite names); add one `CHANGELOG.md` line recording the LOC reduction.

# Acceptance

- `make coverage` prints a before/after LOC table; no `0.0%` production file remains outside §7 keepers
- `! git diff --stat -- TESTING_PLAN.md | grep .` (file untouched)
- `make check && make docs-check && make zed-check` all exit `0`
- `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test` on `165.232.86.174` exits `0` and leaves `/home/slipfork/demo` serving `:80` (`curl -s localhost:80 | head -1` non-empty)
- `grep -rn '0\.1\.0:\|Option A vs Option B' crates/*/src --include='*.rs' | wc -l` is `0`
- `uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate`
