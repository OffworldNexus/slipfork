---
id: M3.73
title: "Move the slipfork-lsp darwin/aarch64 archive leg to macos-14 (macos-15 arm64 pool cancels queued jobs after 15 min)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: [".github/workflows/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T20:16"
updated: "2026-11-23T20:16"
---

# Problem

`packages.yml` -> `slipfork-lsp archive (darwin/aarch64)` uses `runs_on: macos-15` (arm64). Three consecutive attempts (137917761621, 137927410758, 137936957626) all ended `conclusion: cancelled` with an empty `runner_name`, `steps: []`, exactly 15 min after `started_at` — GitHub never scheduled a runner. `CI`'s `lsp + forkfile (darwin/aarch64)` (same label) passed in 43s in the same window, so the pool is intermittently starved rather than the build being broken.

# Change

- `.github/workflows/packages.yml` matrix (`slipfork-lsp archive`): `platform: darwin / arch: aarch64` -> `runs_on: macos-14` (also arm64; a different pool, and a lower macOS floor for the *shipped* archive is a plus).
- Keep `CI`'s certification matrix on `macos-15` (it builds, tests, and runs the server) and keep `darwin/x86_64` on `macos-15-intel`.
- Add a short comment in the matrix explaining the split: CI certifies on `macos-15`; the distribution archive uses `macos-14` so a single starved pool cannot block the release artifacts.

# Acceptance

- `gh run list --workflow=packages.yml` on the pushed commit shows `slipfork-lsp archive (darwin/aarch64)` **pass** on a real `macos-14` runner (`runner_name` non-empty), with all 26 `Packages` jobs green.
- No other workflow job changes label; `Docs` and `CI` stay green.
