# Log M3.32.9

## 2026-11-19T16:00 [lead]

created (task) Migrate docs: docs/reference/cli.md, getting-started, CHANGELOG + legacy demo migration rehearsal

## 2026-11-19T22:25 [lead]

status ready -> active

## 2026-11-19T22:25 [lead]

attempt 1/3 started

## 2026-11-19T22:25 [lead]

base f81072a2f76471366c314c4076e9dbc6ea2b5045

## 2026-11-19T22:26 [writer]

Rehearsed migration on the real binary from /tmp/opencode/m3-32-9-migrate: `slipfork migrate` => `✓ Migrated snow.yml → Forkfile (2 services, 1 postgres cluster, 2 env groups)`, `--json migrate` => {"env_groups":2,"input":"snow.yml","ok":true,"output":"Forkfile","pg_clusters":1,"services":2}; first attempt with Snow's `!!` generator form was refused (unsupported template), so the fixture uses `??`. Fixture copied verbatim to support/demo/migrate/snow.yml and re-run there: output matches docs/getting-started.md byte for byte.

## 2026-11-19T22:26 [lead]

status active -> review

## 2026-11-19T22:26 [lead]

edited fields

## 2026-11-19T22:26 [lead]

attempt 2/3 started

## 2026-11-19T22:26 [lead]

Triad fix pass base f81072a2f76471366c314c4076e9dbc6ea2b5045

## 2026-11-19T22:27 [lead]

status review -> done
