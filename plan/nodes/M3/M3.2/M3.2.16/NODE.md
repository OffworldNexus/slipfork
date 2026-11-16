---
id: M3.2.16
title: "Published 0.3.0 docs for slipfork test: CLI reference, testing DAG, README, getting-started recipe"
kind: task
status: ready
owner: "doc-writer"
priority: 4
depends_on: [M3.2.15]
paths: [docs/reference/cli.md, "docs/testing-dag.md", docs/index.md, README.md, "docs/getting-started.md"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Publish the `0.3.0` dream-tool documentation for `slipfork test`: the CLI reference, the testing/fixture DAG page, the home page feature matrix, and the README quickstart — describing only what the shipped binary actually does.

# Context

- Core Rule #4 (`dx-first`): published pages may document a capability only after it runs. By this node `M3.2.12`–`M3.2.15` have shipped `slipfork test`, typed coverage, and the adapters with registry evidence.
- Phase 1 (`M3.18`) already made these pages Btrfs-true; this node adds the testing surface without regressing that.

# Design / Invariants

1. `docs/reference/cli.md`: full `slipfork test` entry — flags (`-j`, `--json`, `--host-caches`, adapter selection), exit codes, the canonical `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` tree, and the `[test]` TOML block copied from a real run.
2. `docs/testing-dag.md`: `[artifacts]`/`[fixtures]` semantics including `per_world = true` (post-origin, never cached), warm-pool behaviour, `batch` vs `world` isolation, and the `RunnerProtocol v1` description with the exact frame examples from `ADR-0006` §3.
3. `docs/index.md` + `README.md`: add the `slipfork test` one-liner and coverage example to the shipped feature matrix; keep prerequisites (Btrfs, `crun`, buildx) exactly as Phase 1 left them; move any remaining "leftover" claim that now ships.
4. `docs/getting-started.md`: a working pytest recipe (`snow.yml` + `slipfork.toml` + `slipfork test -j 4`) using the `pytest-slipfork` adapter, with the coverage report it produces.
5. Zero vaporware: every command/flag/field must exist in the tree; `make docs-check` (Zensical `--strict` + `check_docs_honesty.py`) is the gate. No `[derived]`/`[guessed]` markers.

# Acceptance

```bash
grep -n 'slipfork test' README.md docs/index.md docs/getting-started.md docs/reference/cli.md
grep -n 'runner.sock' docs/reference/cli.md docs/testing-dag.md
grep -n 'coverage_fail_under' docs/reference/cli.md docs/testing-dag.md
grep -n 'per_world' docs/testing-dag.md
grep -n 'pytest-slipfork' docs/getting-started.md
uv run --no-project tools/check_docs_honesty.py
! grep -rn '\[derived\]\|\[guessed\]' README.md docs/
make docs-check
```

Expected: every page names `slipfork test`; the run tree, the threshold key, `per_world`, and the pytest adapter recipe are documented; honesty check passes; the inverted marker `grep` prints nothing; `make docs-check` exits 0.
