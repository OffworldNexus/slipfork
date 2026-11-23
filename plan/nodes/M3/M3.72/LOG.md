# Log M3.72

## 2026-11-23T19:02 [conductor]

created (task) Collapse demo api/worker onto one '@uv run …' command for dev and prod (drop the python-vs-uv else branches)

## 2026-11-23T19:03 [conductor]

edited fields

## 2026-11-23T19:03 [conductor]

status ready -> ready: Widened: bare 'uv run' (hooks, pre-deploy, exec) must also get --no-sync, not just '@uv run'

## 2026-11-23T19:03 [conductor]

status ready -> ready: Widened paths: bare 'uv run' (hooks, pre-deploy, exec) must also get --no-sync

## 2026-11-23T19:03 [conductor]

edited fields

## 2026-11-23T19:04 [lead]

edited fields

## 2026-11-23T19:04 [lead]

status ready -> active

## 2026-11-23T19:04 [lead]

attempt 1/3 started

## 2026-11-23T19:04 [lead]

base c9e0c544eb266a67333885f52e35091dcc054dcf

## 2026-11-23T19:11 [conductor]

edited fields

## 2026-11-23T19:12 [lead]

edited fields

## 2026-11-23T19:12 [lead]

edited fields

## 2026-11-23T19:28 [lead]

status active -> review

## 2026-11-23T19:28 [lead]

Triad: arch/tests/ux findings fixed (tool_env UV_NO_SYNC removed; doc snippets; stage fixture). Local: make check + docs-check + test-tools + guard + critical_guard green. Live 165.232.86.174 on packaged 0.1.0 .deb: world_journey 2/2, fork_live 2/2, status_live 5/5; prod-check -F -dev 200 / + /api/status, api=/app/.venv uvicorn CMD, front=node build, worker=uv run --no-sync, no mailpit, 0 /project mounts; drop -f 236ms; demo+feat-auth dev 200, tralala stopped; VM demo status clean.

## 2026-11-23T19:28 [lead]

status review -> done
