# Log M3.28.14

## 2026-11-17T16:34 [planner]

created (task) Publish the 0.1.0 UX surface: CLI reference, getting started, index, README, CHANGELOG

## 2026-11-17T21:44 [lead]

From M3.28.18 RESULT (route, docs): docs/getting-started.md:119 still explains 'not_offered' as 'no paired OSPP session offered a remote database' although no provider ships in 0.1.0 (rewrite to the local/hermetic wording or point at reference/cli.md#seed-dumps-and-sealed-bases). support/integrations.toml ospp-v1-secrets-and-dumps.recipe_page still targets docs/snow-modes.md; retarget docs/reference/ospp.md and run make support-sync. Also re-check docs/reference/cli.md:109-120 conditional seed-dump prose after M3.28.11 lands -v.

## 2026-11-17T22:43 [lead]

edited fields

## 2026-11-17T22:43 [lead]

quad finding from M3.28.6 folded in: getting-started.md:65,153-164, index.md:18, README.md:27 and snow-modes.md:96 still teach the superseded 'daemon unit | sudo tee' + manual apparmor_parser + $XDG_RUNTIME_DIR/slipfork paths and a --runtime-dir client flag that no longer parses. The single canonical install is now 'slipfork daemon install'. Delete them here (snow-modes.md added to paths) — do not document a second install path.

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T00:38 [lead]

edited fields

## 2026-11-18T00:38 [lead]

added docs/design/do-vm-harness.md: it still documents render_apparmor_body and a harness-written /etc/apparmor.d/slipfork (deleted by M3.28.12) plus pre-AF_NETLINK sandbox text. Sweep it in the same pass as the published pages.

## 2026-11-18T04:34 [lead]

edited fields

## 2026-11-18T04:34 [lead]

added poc/run_benchmark.py: its benchmark still drives 'slipfork dag plan', a command M3.28.11 deleted, so the opt-in 'make bench' is broken. Remove that step or retarget it to a shipped command.

## 2026-11-18T04:59 [lead]

from M3.28.23: docs/reference/cli.md rows 67/105 must document the new logs rule — every line is prefixed '<source>: ' when the request names no single target; a single named target prints raw; --json is NDJSON with {stream, source, text}. Also still pending here: poc/run_benchmark.py's deleted 'dag plan' step, and the do-vm-harness.md rewrite.

## 2026-11-18T05:07 [lead]

from M3.28.23 (record in CHANGELOG as a known limitation, do not hide): 'logs --follow' with no explicit target still follows only the first retained target (pre-existing daemon fan-out limitation; needs thread-per-target). Also state the new logs rule: '<source>: ' prefix when no single target is named, raw bytes for one named target, and NDJSON {stream,source,text} under --json.
