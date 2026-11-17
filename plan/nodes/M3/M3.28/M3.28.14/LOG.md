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
