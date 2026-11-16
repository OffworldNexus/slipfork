---
id: M3.2.11
title: "packages/pytest-slipfork: zero-ceremony pytest plugin (RunnerProtocol v1, warm os.fork Zygote)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.2.3]
paths: ["packages/pytest-slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Ship `packages/pytest-slipfork`: a zero-ceremony pytest plugin that speaks `RunnerProtocol v1` over `.slipfork/runs/<run_id>/<slot>/runner.sock`, reuses the world's migrated/seeded/routed stack, and forks a warm Zygote (`os.fork()` after app/framework import) in under `1 ms` per run.

# Context

- This is Adapter #1 (`ADR-0006` §2–§4). Its whole value is subtracting `test_<db>` creation, `live_server`, `serialized_rollback`, and per-run framework import cost.
- It runs inside the world on the world's own Python (from the project `Dockerfile` stage), so the package must be importable without installation ceremony (`python -m pytest_slipfork` / `pytest -p pytest_slipfork`), and must never read `snow.yml` or resolve env itself.

# Design / Invariants

1. Package layout: `packages/pytest-slipfork/{pyproject.toml,src/pytest_slipfork/*.py,tests/*.py,README.md}`; stdlib + `pytest` only (no Django import at module scope), `requires-python >= 3.10`, entry point declares the pytest plugin.
2. Transport: bind `runner.sock` (path injected via `--slipfork-socket` / env owned by the adapter), serve NDJSON on one connection at a time, `discover` → collected node ids mapped to `{id, fixture, isolation}` (fixture/isolation come from typed CLI/env inputs the adapter supplies — never guessed from test names), `run` → execute the requested node ids and stream `item_result` frames then exactly one `run_complete`.
3. Zygote: import the app/framework once before accepting `run` (setting up Django with the world's `DJANGO_SETTINGS_MODULE` when present), then `os.fork()` per `run` request, reporting the child's results over the inherited socket and exiting the child immediately; the parent stays warm for the next request. Document that this is why results arrive in `<1 ms` after the frame.
4. Coverage: honour the adapter-supplied `COVERAGE_FILE`/coverage directory; never spawn a shell; exit codes and tracebacks are returned in the `item_result.message` (bounded) so the Rust side reports them verbatim.
5. Tests: `packages/pytest-slipfork/tests/` cover frame parsing (unknown op/version), discover shape, a passing and a failing item, the forked-child path (assert both the child's result stream and that the parent survives), and that no shell/subprocess is used. Run them with `uv run --no-project --with pytest pytest -q packages/pytest-slipfork/tests`.

# Acceptance

```bash
uv run --no-project --with pytest pytest -q packages/pytest-slipfork/tests
uvx --offline ruff check packages/pytest-slipfork && uvx --offline ruff format --check packages/pytest-slipfork
uv run --no-project python -c 'import sys; sys.path.insert(0, "packages/pytest-slipfork/src"); import pytest_slipfork; print(pytest_slipfork.PROTOCOL_VERSION)'
grep -n 'os.fork' packages/pytest-slipfork/src/pytest_slipfork/*.py
grep -n 'runner.sock\|CONNECT\|JSON' packages/pytest-slipfork/src/pytest_slipfork/*.py
! grep -rn 'subprocess\|shell=True' packages/pytest-slipfork/src/pytest_slipfork/
```

Expected: plugin tests, both `ruff` commands, and the import smoke check exit 0 (the import prints `1`); `os.fork` and the socket/NDJSON handling exist; the inverted `grep` prints nothing (no subprocess/shell use in the plugin).
