# Log M3.50.5

## 2026-11-22T12:41 [planner]

created (task) Docs/manpage/CHANGELOG: purge db+redis, document shell <target>/logs/TUI

## 2026-11-22T12:42 [lead]

edited fields

## 2026-11-22T13:07 [lead]

edited fields

## 2026-11-22T13:07 [lead]

OWNER ADDENDUM: document the 'orphaned' status state (yellow, next: slipfork up to reconcile) and that repeating slipfork up after a Forkfile edit reconciles stopped/added services instead of no-oping.

## 2026-11-22T13:13 [lead]

edited fields

## 2026-11-22T13:13 [lead]

OWNER ADDENDUM: specs/01+02 must lock the domain-first tree (flat worlds, domains->paths, zero ports/ages) and the always-<bucket>.<world>.localhost S3 domain with `cdn <id>` resolving to the bucket domain locally; docs/README/demo README follow the same copy.

## 2026-11-22T13:15 [lead]

OWNER REFINEMENT: specs/02 + docs/cli.md show the uniform domain->path->target shape for ALL domains including S3 buckets (no special-case inline bucket rows).

## 2026-11-22T13:18 [lead]

status ready -> active

## 2026-11-22T13:18 [lead]

attempt 1/3 started

## 2026-11-22T13:18 [lead]

base fe0c1c8aeb39a2e3ff81a82ac9fa9d14c36e3873

## 2026-11-22T13:21 [lead]

OWNER ADDENDUM (logs): document the timestamped canonical log index (<id>.idx, 16-byte (ts_ms, offset) entries, O(log N) seek) and the new CLI: slipfork logs [TARGETS]... (0/1/many), --since/--until (relative 30s|5m|2h|1d and RFC3339), -t/--timestamps; ts_ms always in --json. Regenerate the manpage AFTER M3.50.8 lands so flags match.

## 2026-11-22T13:22 [lead]

edited fields

## 2026-11-22T13:22 [lead]

edited fields

## 2026-11-22T13:23 [lead]

status active -> done
