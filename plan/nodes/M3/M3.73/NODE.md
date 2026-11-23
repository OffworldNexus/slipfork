---
id: M3.73
title: "Move the slipfork-lsp darwin/aarch64 archive leg to macos-26 (macos-15 arm64 pool cancels queued jobs after 15 min)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [".github/workflows/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-23T20:16"
updated: "2026-11-23T20:47"
---

# Problem

`packages.yml` -> `slipfork-lsp archive (darwin/aarch64)` uses `runs_on: macos-15` (arm64). Three consecutive attempts (137917761621, 137927410758, 137936957626) all ended `conclusion: cancelled` with an empty `runner_name`, `steps: []`, exactly 15 min after `started_at` — GitHub never scheduled a runner. `CI`'s `lsp + forkfile (darwin/aarch64)` (same label) passed in 43s in the same window, so the pool is intermittently starved rather than the build being broken.

# Change

- `.github/workflows/packages.yml` matrix (`slipfork-lsp archive`): `platform: darwin / arch: aarch64` -> `runs_on: macos-26` (arm64; `macos-14` was rejected 1s later as x64/retired).
- Keep `CI`'s certification matrix on `macos-15` (it builds, tests, and runs the server) and keep `darwin/x86_64` on `macos-15-intel`.
- Add a short comment in the matrix explaining the split: CI certifies on `macos-15`; the distribution archive uses `macos-26` so a single starved pool cannot block the release artifacts.

# Verified (ddbae7f, run 45248545947 attempt 2)

- `slipfork-lsp archive (darwin/aarch64)` **success** on `labels=["macos-26"]`, `runner_name=GitHub Actions 1000009798`, 30s; archive `--version` step success on native arm64.
- Attempt 1 starved 15 min (empty `runner_name`, GitHub "not acquired by Runner of type hosted"); attempt 2 acquired in seconds — pool capacity is intermittent, the label itself works.
- Mach-O floor unchanged: macos-26 artifact `LC_BUILD_VERSION platform macos, sdk 26.5, minos 11.0` (macos-15 artifact: sdk 15.5, minos 11.0), so **no** `MACOSX_DEPLOYMENT_TARGET` pin was added.

# Acceptance

- `gh run list --workflow=packages.yml` on `ddbae7f` shows all 26 `Packages` jobs green, including `slipfork-lsp archive (darwin/aarch64)` on a real `macos-26` runner (`runner_name` non-empty).
- No other workflow job changes label; `Docs` and `CI` stay green on `ddbae7f`.
